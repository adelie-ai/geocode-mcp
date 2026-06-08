#![deny(warnings)]

use serde_json::{Value, json};
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

    /// Call a tool and return Ok(result) for success or Err(message) for both
    /// JSON-RPC errors and MCP-level tool errors (`isError: true`).
    fn tool_call(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        let resp = self.call("tools/call", json!({"name":name,"arguments":arguments}))?;
        let result = resp
            .get("result")
            .cloned()
            .ok_or_else(|| format!("missing result field: {resp}"))?;

        // Per MCP spec, tool errors come back as isError:true in the result body —
        // surface them as Err so test assertions stay natural.
        if result.get("isError") == Some(&Value::Bool(true)) {
            let msg = result
                .get("content")
                .and_then(|c| c.as_array())
                .and_then(|arr| arr.first())
                .and_then(|entry| entry.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("tool error")
                .to_string();
            return Err(msg);
        }

        Ok(result)
    }
}

impl Drop for McpStdioClient {
    fn drop(&mut self) {
        let _ = self.call("shutdown", json!({}));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Extract the JSON payload from the first `type: text` content entry.
///
/// The server now emits `{"type":"text","text":"<json string>"}` (MCP spec).
fn extract_text_as_value(tool_result: &Value) -> Value {
    let content = tool_result
        .get("content")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("expected result.content array, got: {tool_result}"));

    for entry in content {
        if entry.get("type") == Some(&Value::String("text".to_string())) {
            let text = entry
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or_else(|| panic!("text entry missing 'text' field: {entry}"));
            return serde_json::from_str(text)
                .unwrap_or_else(|e| panic!("text content is not valid JSON: {e}\n  text: {text}"));
        }
    }

    panic!("no text content entry in: {tool_result}");
}

fn network_tests_enabled() -> bool {
    std::env::var("RUN_NETWORK_TESTS").ok().as_deref() == Some("1")
}

fn expect_err_contains<T: std::fmt::Debug>(res: Result<T, String>, needle: &str) {
    match res {
        Ok(v) => panic!("expected error containing '{needle}', but call succeeded: {v:?}"),
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

/// The server must respond to `initialize` with serverInfo and capabilities,
/// and must NOT include a non-standard top-level `tools` key.
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
    // Non-standard `tools` key must be absent from initialize response
    assert!(
        result.get("tools").is_none(),
        "initialize result must not contain top-level 'tools' key: {result}"
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

    let expected = ["geocode", "reverse_geocode"];

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

/// An unknown tool name must return a tool error (isError:true), not a JSON-RPC error.
#[test]
fn test_unknown_tool_returns_is_error() {
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

/// `geocode` must reject an empty name.
#[test]
fn test_geocode_empty_name_rejected() {
    let mut client = McpStdioClient::start();
    client.initialize();
    let result = client.tool_call("geocode", json!({"name": ""}));
    expect_err_contains(result, "empty");
}

/// `geocode` must reject a whitespace-only name.
#[test]
fn test_geocode_whitespace_name_rejected() {
    let mut client = McpStdioClient::start();
    client.initialize();
    let result = client.tool_call("geocode", json!({"name": "   "}));
    expect_err_contains(result, "empty");
}

/// `reverse_geocode` must reject missing latitude.
#[test]
fn test_reverse_geocode_missing_latitude() {
    let mut client = McpStdioClient::start();
    client.initialize();
    let result = client.tool_call("reverse_geocode", json!({"longitude": -0.1278}));
    expect_err_contains(result, "latitude");
}

/// `reverse_geocode` must reject missing longitude.
#[test]
fn test_reverse_geocode_missing_longitude() {
    let mut client = McpStdioClient::start();
    client.initialize();
    let result = client.tool_call("reverse_geocode", json!({"latitude": 51.5074}));
    expect_err_contains(result, "longitude");
}

// ── httpmock-based unit tests (run unconditionally — no external network needed) ─

#[cfg(test)]
mod httpmock_tests {
    use geocode_mcp::operations::geocode::geocode_location_with_base;
    use geocode_mcp::operations::reverse_geocode::reverse_geocode_location_with_base;
    use httpmock::prelude::*;
    use std::time::Duration;

    fn test_client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .expect("reqwest client")
    }

    /// Zero-feature response must produce an isError-style LocationNotFound error.
    #[tokio::test]
    async fn test_geocode_empty_features_is_location_not_found() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api/");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(r#"{"features":[]}"#);
            })
            .await;

        let client = test_client();
        let base = format!("http://{}/api/", server.address());
        let result = geocode_location_with_base(&client, &base, "Atlantis", 5, None).await;

        assert!(result.is_err(), "expected LocationNotFound error");
        let err = result.unwrap_err().to_string();
        assert!(
            err.to_lowercase().contains("not found"),
            "expected 'not found' in error, got: {err}"
        );
    }

    /// Malformed JSON from Photon (missing geometry.coordinates) must be handled gracefully.
    #[tokio::test]
    async fn test_geocode_missing_coordinates_filtered_out() {
        let server = MockServer::start_async().await;
        // Feature with coordinates array too short; should be filtered and trigger LocationNotFound
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api/");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(
                        r#"{
                            "features": [
                                {
                                    "properties": {"name": "Broken", "country": "Nowhere"},
                                    "geometry": {"coordinates": []}
                                }
                            ]
                        }"#,
                    );
            })
            .await;

        let client = test_client();
        let base = format!("http://{}/api/", server.address());
        let result = geocode_location_with_base(&client, &base, "Broken", 5, None).await;

        assert!(result.is_err(), "expected error for missing coordinates");
    }

    /// HTTP 429 from Photon must surface as an ApiError, not a panic or timeout.
    #[tokio::test]
    async fn test_geocode_http_429_propagates() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api/");
                then.status(429);
            })
            .await;

        let client = test_client();
        let base = format!("http://{}/api/", server.address());
        let result = geocode_location_with_base(&client, &base, "London", 5, None).await;

        assert!(result.is_err(), "expected error for HTTP 429");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("429"),
            "expected 429 in error message, got: {err}"
        );
    }

    /// HTTP 503 from Photon must surface as an ApiError.
    #[tokio::test]
    async fn test_geocode_http_503_propagates() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api/");
                then.status(503);
            })
            .await;

        let client = test_client();
        let base = format!("http://{}/api/", server.address());
        let result = geocode_location_with_base(&client, &base, "London", 5, None).await;

        assert!(result.is_err(), "expected error for HTTP 503");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("503"),
            "expected 503 in error message, got: {err}"
        );
    }

    /// count must be clamped: 0 → 1, 11 → 10.
    #[tokio::test]
    async fn test_geocode_count_clamping() {
        let server = MockServer::start_async().await;

        // Accept any limit value; return a single feature
        let feature = r#"{
            "features": [{
                "properties": {"name": "London", "country": "United Kingdom", "countrycode": "GB", "state": "England", "type": "city"},
                "geometry": {"coordinates": [-0.1278, 51.5074]}
            }]
        }"#;

        server
            .mock_async(|when, then| {
                // count=0 should be clamped to 1
                when.method(GET).path("/api/").query_param("limit", "1");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(feature);
            })
            .await;
        server
            .mock_async(|when, then| {
                // count=11 should be clamped to 10
                when.method(GET).path("/api/").query_param("limit", "10");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(feature);
            })
            .await;

        let client = test_client();
        let base = format!("http://{}/api/", server.address());

        let r0 = geocode_location_with_base(&client, &base, "London", 0, None).await;
        assert!(r0.is_ok(), "count=0 clamped to 1 should succeed: {r0:?}");

        let r11 = geocode_location_with_base(&client, &base, "London", 11, None).await;
        assert!(
            r11.is_ok(),
            "count=11 clamped to 10 should succeed: {r11:?}"
        );
    }

    /// A valid Photon response must parse into expected fields.
    #[tokio::test]
    async fn test_geocode_valid_response_parsed() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api/");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(
                        r#"{
                            "features": [{
                                "properties": {
                                    "name": "London",
                                    "country": "United Kingdom",
                                    "countrycode": "GB",
                                    "state": "England",
                                    "type": "city"
                                },
                                "geometry": {"coordinates": [-0.1278, 51.5074]}
                            }]
                        }"#,
                    );
            })
            .await;

        let client = test_client();
        let base = format!("http://{}/api/", server.address());
        let result = geocode_location_with_base(&client, &base, "London", 1, None)
            .await
            .expect("geocode should succeed");

        let arr = result.as_array().expect("result should be array");
        assert_eq!(arr.len(), 1);
        let loc = &arr[0];
        assert_eq!(loc["name"], "London");
        assert_eq!(loc["country_code"], "GB");
        // GeoJSON [lon, lat] → mapped correctly
        assert!((loc["latitude"].as_f64().unwrap() - 51.5074).abs() < 0.001);
        assert!((loc["longitude"].as_f64().unwrap() - (-0.1278)).abs() < 0.001);
    }

    /// reverse_geocode: HTTP 429 must propagate as an error.
    #[tokio::test]
    async fn test_reverse_geocode_http_429_propagates() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/reverse");
                then.status(429);
            })
            .await;

        let client = test_client();
        let base = format!("http://{}/reverse", server.address());
        let result =
            reverse_geocode_location_with_base(&client, &base, 51.5074, -0.1278, None).await;

        assert!(result.is_err(), "expected error for HTTP 429");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("429"),
            "expected 429 in error message, got: {err}"
        );
    }

    /// reverse_geocode: empty features must produce LocationNotFound.
    #[tokio::test]
    async fn test_reverse_geocode_empty_features_is_location_not_found() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/reverse");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(r#"{"features":[]}"#);
            })
            .await;

        let client = test_client();
        let base = format!("http://{}/reverse", server.address());
        let result =
            reverse_geocode_location_with_base(&client, &base, 51.5074, -0.1278, None).await;

        assert!(result.is_err(), "expected LocationNotFound");
        let err = result.unwrap_err().to_string();
        assert!(
            err.to_lowercase().contains("not found"),
            "expected 'not found', got: {err}"
        );
    }

    /// reverse_geocode: valid response must parse correctly.
    #[tokio::test]
    async fn test_reverse_geocode_valid_response_parsed() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/reverse");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(
                        r#"{
                            "features": [{
                                "properties": {
                                    "name": "London",
                                    "country": "United Kingdom",
                                    "countrycode": "GB",
                                    "state": "England",
                                    "type": "city"
                                },
                                "geometry": {"coordinates": [-0.1278, 51.5074]}
                            }]
                        }"#,
                    );
            })
            .await;

        let client = test_client();
        let base = format!("http://{}/reverse", server.address());
        let result = reverse_geocode_location_with_base(&client, &base, 51.5074, -0.1278, None)
            .await
            .expect("reverse geocode should succeed");

        assert_eq!(result["name"], "London");
        assert_eq!(result["country_code"], "GB");
        assert!((result["latitude"].as_f64().unwrap() - 51.5074).abs() < 0.001);
        assert!((result["longitude"].as_f64().unwrap() - (-0.1278)).abs() < 0.001);
    }
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

    let locations = extract_text_as_value(&result);
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

/// Geocode an unknown location must return an isError response (not JSON-RPC error).
#[test]
fn test_geocode_nonexistent_location_network() {
    if !network_tests_enabled() {
        eprintln!("Skipping network test (set RUN_NETWORK_TESTS=1 to enable)");
        return;
    }

    let mut client = McpStdioClient::start();
    client.initialize();

    let result = client.tool_call("geocode", json!({"name": "xyzzy_nonexistent_place_00000"}));
    assert!(
        result.is_err(),
        "expected isError for nonexistent location, but got success"
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

    let locations = extract_text_as_value(&result);
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
