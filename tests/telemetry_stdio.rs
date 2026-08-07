#![deny(warnings)]

// Acceptance tests for the telemetry geocode-mcp inherits from mcp-core's
// `run`: the stdio transport keeps stdout clean at any log level, and a
// caller-supplied place name or coordinate never reaches an INFO line (D10,
// the level contract).
//
// Each test spawns the real binary. Only a real process proves what reaches
// file descriptor 1 and what the installed subscriber really writes to
// stderr; an in-process capturing layer (tests/telemetry_span_fields.rs)
// proves the complementary thing: what reaches a span *field*, which never
// shows up in console text at all unless an event fires inside that span
// (lesson 7, mcp-core#40).
//
// Neither test calls the live Photon API. Both tool calls below are missing
// their other required coordinate/name field, so geocode-mcp's own
// parameter validation rejects them before any outbound request is made —
// the sentinel travels in a second argument instead, which proves the same
// property: mcp-core's dispatch layer logs the whole `arguments` object at
// DEBUG before the tool handler ever runs (server.rs: `tool call
// arguments`), regardless of which key in that object carries the value.

use serde_json::{Value, json};
use std::io::Write;
use std::process::{Child, Command, Output, Stdio};

/// A place name a caller might supply. Carried in `language` on a request
/// whose `name` is omitted, so the call fails validation (missing name)
/// before any network access, while the sentinel still reaches the raw
/// arguments object mcp-core logs at DEBUG.
const SENTINEL_ADDRESS: &str = "MARKER-9f3d1c2a-sentinel-address";

/// A coordinate a caller might supply. Carried directly as `longitude` on a
/// request whose `latitude` is omitted, so the call fails validation
/// (missing latitude) before any network access.
const SENTINEL_LONGITUDE: f64 = -98.765432;

fn spawn_with_log_level(level: &str) -> Child {
    let exe = env!("CARGO_BIN_EXE_geocode-mcp");
    Command::new(exe)
        .args(["serve", "--mode", "stdio"])
        .env("RUST_LOG", level)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn geocode-mcp serve --mode stdio")
}

fn run_requests(level: &str, requests: &[Value]) -> Output {
    let mut child = spawn_with_log_level(level);
    {
        let stdin = child.stdin.as_mut().expect("child has a piped stdin");
        for request in requests {
            writeln!(stdin, "{request}").expect("write jsonrpc line");
        }
    }
    drop(child.stdin.take());
    child.wait_with_output().expect("child must exit")
}

/// The level word `tracing_subscriber`'s default console formatter writes as
/// the second whitespace-separated token, right after the timestamp. Reading
/// it this way (rather than a substring search for "INFO") does not confuse
/// a level word for content that happens to contain the same letters.
fn line_level(line: &str) -> Option<&str> {
    line.split_whitespace()
        .nth(1)
        .filter(|token| matches!(*token, "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE"))
}

fn geocode_request(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": "geocode", "arguments": {"count": 3, "language": SENTINEL_ADDRESS}},
    })
}

fn reverse_geocode_request(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": "reverse_geocode", "arguments": {"longitude": SENTINEL_LONGITUDE}},
    })
}

#[test]
fn stdout_carries_only_jsonrpc_at_trace_level() {
    let requests = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        geocode_request(3),
        reverse_geocode_request(4),
        json!({"jsonrpc":"2.0","id":5,"method":"shutdown","params":{}}),
    ];
    let output = run_requests("trace", &requests);
    assert!(
        output.status.success(),
        "geocode-mcp must exit cleanly, otherwise an empty stdout proves nothing: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let mut replies = 0;
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line).unwrap_or_else(|e| {
            panic!("every stdout line must be JSON-RPC, but {line:?} is not: {e}")
        });
        assert_eq!(
            value.get("jsonrpc").and_then(Value::as_str),
            Some("2.0"),
            "every stdout line must carry the JSON-RPC envelope: {line:?}"
        );
        replies += 1;
    }
    assert_eq!(
        replies, 5,
        "expected one reply per request that carried an id"
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(
        stderr.contains("INFO") || stderr.contains("DEBUG") || stderr.contains("TRACE"),
        "at RUST_LOG=trace the subscriber must be installed and log to stderr; stderr was: \
         {stderr:?}"
    );
}

/// AC (mcp-core#40, D10): no place name and no coordinate reaches an INFO
/// (or higher) line, for either tool. The failure path is what is driven
/// here, so the sentinel is present in the raw arguments regardless of
/// what geocode-mcp's own validation does with them.
#[test]
fn no_address_or_coordinate_reaches_an_info_line_on_the_failure_path() {
    let requests = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
        geocode_request(2),
        reverse_geocode_request(3),
        json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":{}}),
    ];
    let output = run_requests("trace", &requests);
    assert!(
        output.status.success(),
        "geocode-mcp must exit cleanly: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    let longitude_text = SENTINEL_LONGITUDE.to_string();
    let mut saw_address_at_debug = false;
    let mut saw_longitude_at_debug = false;

    for line in stderr.lines() {
        let carries_address = line.contains(SENTINEL_ADDRESS);
        let carries_longitude = line.contains(&longitude_text);
        if !carries_address && !carries_longitude {
            continue;
        }
        let level = line_level(line);
        assert!(
            matches!(level, Some("DEBUG") | Some("TRACE")),
            "a place name or coordinate reached a line at level {level:?}, at or above INFO: \
             {line:?}"
        );
        if level == Some("DEBUG") {
            saw_address_at_debug |= carries_address;
            saw_longitude_at_debug |= carries_longitude;
        }
    }

    assert!(
        saw_address_at_debug,
        "the place name must still be reachable at DEBUG, or this test cannot tell a real fix \
         from a line that was simply deleted; stderr was: {stderr:?}"
    );
    assert!(
        saw_longitude_at_debug,
        "the coordinate must still be reachable at DEBUG, or this test cannot tell a real fix \
         from a line that was simply deleted; stderr was: {stderr:?}"
    );
}
