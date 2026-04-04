#![deny(warnings)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

// ── MCP stdio harness ─────────────────────────────────────────────────────────

struct McpStdioClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl McpStdioClient {
    fn start() -> Self {
        let exe = env!("CARGO_BIN_EXE_geocode-mcp");

        let mut child = Command::new(exe)
            .args(["serve", "--mode", "stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn geocode-mcp serve --mode stdio");

        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");

        Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        }
    }

    fn send(&mut self, obj: &Value) {
        let s = serde_json::to_string(obj).expect("serialize jsonrpc");
        self.stdin
            .write_all(s.as_bytes())
            .and_then(|_| self.stdin.write_all(b"\n"))
            .and_then(|_| self.stdin.flush())
            .expect("write jsonrpc line");
    }

    fn read_msg(&mut self) -> Value {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.stdout.read_line(&mut line).expect("read line");
            if n == 0 {
                panic!("mcp server closed stdout");
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                return v;
            }
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;

        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));

        loop {
            let msg = self.read_msg();
            if msg.get("id").and_then(|v| v.as_u64()) != Some(id) {
                continue;
            }
            if let Some(err) = msg.get("error") {
                return Err(err.to_string());
            }
            return Ok(msg);
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc":"2.0","method":method,"params":params}));
    }

    fn initialize(&mut self) {
        self.call(
            "initialize",
            json!({"protocolVersion":"2025-11-25","capabilities":{}}),
        )
        .expect("initialize");
        self.notify("initialized", json!({}));
    }

    fn tool_call(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        let resp = self.call("tools/call", json!({"name":name,"arguments":arguments}))?;
        resp.get("result")
            .cloned()
            .ok_or_else(|| format!("missing result field: {resp}"))
    }
}

impl Drop for McpStdioClient {
    fn drop(&mut self) {
        let _ = self.call("shutdown", json!({}));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Extract the `value` field from the first `type: json` content entry.
fn extract_value(tool_result: &Value) -> Value {
    let content = tool_result
        .get("content")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("expected result.content array, got: {tool_result}"));

    for entry in content {
        if entry.get("type") == Some(&Value::String("json".to_string())) {
            if let Some(v) = entry.get("value") {
                return v.clone();
            }
        }
    }

    panic!("no json content entry in: {tool_result}");
}

fn network_tests_enabled() -> bool {
    std::env::var("RUN_NETWORK_TESTS").ok().as_deref() == Some("1")
}

fn expect_err_contains<T>(res: Result<T, String>, needle: &str) {
    match res {
        Ok(_) => panic!("expected error containing '{needle}', but call succeeded"),
        Err(e) => {
            let lower = e.to_lowercase();
            assert!(
                lower.contains(&needle.to_lowercase()),
                "expected error containing '{needle}', got: {e}"
            );
        }
    }
}

// ── Protocol tests (no network) ───────────────────────────────────────────────

/// The server must respond to `initialize` with serverInfo and capabilities.
#[test]
fn test_initialize_response_shape() {
    let mut client = McpStdioClient::start();
    let resp = client
        .call(
            "initialize",
            json!({"protocolVersion":"2025-11-25","capabilities":{}}),
        )
        .expect("initialize");

    let result = resp.get("result").expect("result field");
    assert!(
        result.get("serverInfo").is_some(),
        "missing serverInfo: {result}"
    );
    let server_info = result.get("serverInfo").unwrap();
    assert_eq!(
        server_info.get("name").and_then(|v| v.as_str()),
        Some("geocode-mcp"),
        "unexpected serverInfo.name"
    );
    assert!(
        result.get("capabilities").is_some(),
        "missing capabilities: {result}"
    );
}

/// `tools/list` must return the expected set of tool names.
#[test]
fn test_tools_list_contains_expected_tools() {
    let mut client = McpStdioClient::start();
    client.initialize();

    let resp = client.call("tools/list", json!({})).expect("tools/list");
    let result = resp.get("result").expect("result field");

    let tools_val = result.get("tools").expect("tools field");
    let tools = match tools_val.as_array() {
        Some(arr) => arr,
        None => {
            panic!("tools is not an array: {tools_val}");
        }
    };

    // Flatten one level if tools is [[{...}, {...}, ...]]
    let names: Vec<&str> = if tools.len() == 1 && tools[0].is_array() {
        tools[0]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
            .collect()
    } else {
        tools
            .iter()
            .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
            .collect()
    };

    let expected = ["geocode"];

    for expected_name in &expected {
        assert!(
            names.contains(expected_name),
            "tool '{}' missing from tools/list. Got: {:?}",
            expected_name,
            names
        );
    }
}

/// Calling a tool before `initialize` must return an error.
#[test]
fn test_tool_call_before_initialize_returns_error() {
    let mut client = McpStdioClient::start();
    let result = client.tool_call("geocode", json!({"name": "London"}));
    assert!(
        result.is_err(),
        "expected error when calling tool before initialize"
    );
}

/// An unknown tool name must return a ToolNotFound error.
#[test]
fn test_unknown_tool_returns_error() {
    let mut client = McpStdioClient::start();
    client.initialize();
    let result = client.tool_call("nonexistent_tool", json!({}));
    expect_err_contains(result, "not found");
}

/// An unknown method must return a method-not-found error.
#[test]
fn test_unknown_method_returns_method_not_found() {
    let mut client = McpStdioClient::start();
    client.initialize();
    let result = client.call("unknownMethod/foobar", json!({}));
    assert!(result.is_err(), "expected error for unknown method");
}

/// Malformed JSON must return a parse error.
#[test]
fn test_malformed_json_returns_parse_error() {
    let mut client = McpStdioClient::start();

    client
        .stdin
        .write_all(b"this is not json at all\n")
        .and_then(|_| client.stdin.flush())
        .expect("write malformed json");

    let msg = client.read_msg();
    assert!(
        msg.get("error").is_some(),
        "expected error response for malformed json, got: {msg}"
    );
}

// ── Parameter validation tests (no network) ───────────────────────────────────

/// `geocode` must reject a missing name.
#[test]
fn test_geocode_missing_name() {
    let mut client = McpStdioClient::start();
    client.initialize();
    let result = client.tool_call("geocode", json!({"count": 3}));
    expect_err_contains(result, "name");
}

// ── Network integration tests (require RUN_NETWORK_TESTS=1) ──────────────────

/// Geocode "London" and verify we get a plausible UK result.
#[test]
fn test_geocode_london_network() {
    if !network_tests_enabled() {
        eprintln!("Skipping network test (set RUN_NETWORK_TESTS=1 to enable)");
        return;
    }

    let mut client = McpStdioClient::start();
    client.initialize();

    let result = client
        .tool_call("geocode", json!({"name": "London", "count": 3}))
        .expect("geocode London");

    let locations = extract_value(&result);
    let arr = locations.as_array().expect("expected array of locations");
    assert!(!arr.is_empty(), "expected at least one geocode result");

    let first = &arr[0];
    assert_eq!(
        first.get("name").and_then(|v| v.as_str()),
        Some("London"),
        "first result name should be London"
    );

    let lat = first.get("latitude").and_then(|v| v.as_f64()).unwrap();
    let lon = first.get("longitude").and_then(|v| v.as_f64()).unwrap();
    assert!(
        (lat - 51.5).abs() < 1.0,
        "unexpected latitude for London: {}",
        lat
    );
    assert!(
        (lon - (-0.12)).abs() < 1.0,
        "unexpected longitude for London: {}",
        lon
    );
}

/// Geocode an unknown location must return a LocationNotFound error.
#[test]
fn test_geocode_nonexistent_location_network() {
    if !network_tests_enabled() {
        eprintln!("Skipping network test (set RUN_NETWORK_TESTS=1 to enable)");
        return;
    }

    let mut client = McpStdioClient::start();
    client.initialize();

    let result = client.tool_call(
        "geocode",
        json!({"name": "xyzzy_nonexistent_place_00000"}),
    );
    assert!(
        result.is_err(),
        "expected error for nonexistent location, but got success"
    );
}

/// Geocode with language parameter.
#[test]
fn test_geocode_with_language_network() {
    if !network_tests_enabled() {
        eprintln!("Skipping network test (set RUN_NETWORK_TESTS=1 to enable)");
        return;
    }

    let mut client = McpStdioClient::start();
    client.initialize();

    let result = client
        .tool_call(
            "geocode",
            json!({"name": "Tokyo", "count": 1, "language": "ja"}),
        )
        .expect("geocode Tokyo in Japanese");

    let locations = extract_value(&result);
    let arr = locations.as_array().expect("expected array of locations");
    assert!(!arr.is_empty(), "expected at least one geocode result");

    let first = &arr[0];
    let lat = first.get("latitude").and_then(|v| v.as_f64()).unwrap();
    assert!(
        (lat - 35.69).abs() < 1.0,
        "unexpected latitude for Tokyo: {}",
        lat
    );
}
