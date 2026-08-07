#![deny(warnings)]

// Acceptance tests for the telemetry geocode-mcp inherits from mcp-core's
// `run`: the stdio transport keeps stdout clean at any log level, and a
// caller-supplied place name or coordinate never reaches an INFO line (D10,
// the level contract).
//
// Each test spawns the real binary as a separate OS process. Only a real
// process proves what reaches file descriptor 1 and what the installed
// subscriber really writes to stderr; an in-process capturing layer
// (tests/telemetry_span_fields.rs) proves the complementary thing: what
// reaches a span *field*, which never shows up in console text at all
// unless an event fires inside that span (lesson 7, mcp-core#40).
//
// This file does NOT use `support::tool_probes()`. That table carries
// valid, sentinel-bearing arguments so `tests/telemetry_span_fields.rs` can
// point the service at a local mock and reach the live outbound-request
// code (mcp-core#40 lesson 9). A spawned OS process has no such mock to
// reach -- it is the real, unmodified binary -- so driving it with the same
// valid arguments would make this test call the live Photon API, which the
// ticket forbids. The probes below stay deliberately invalid instead (a
// required field is missing, so geocode-mcp's own validation rejects the
// call before any outbound request), which is sufficient for what this
// file actually proves: process-level stdout/stderr hygiene under the real,
// installed subscriber. The sentinel still reaches mcp-core's own raw
// arguments log (DEBUG), which is what supplies this file's positive
// control.

mod support;

use serde_json::{Value, json};
use std::io::Write;
use std::process::{Child, Command, Output, Stdio};

/// One tool's request for this file's process-hygiene tests: deliberately
/// invalid (missing a required field), so it is safe to send to the real,
/// unmodified binary.
struct StdioProbe {
    tool: &'static str,
    arguments: Value,
    sentinels: Vec<String>,
}

fn stdio_probes() -> Vec<StdioProbe> {
    vec![
        StdioProbe {
            tool: "geocode",
            // `name` is omitted so this call fails validation (missing
            // name) before any network access; the sentinel travels
            // through `language` instead, which still reaches mcp-core's
            // raw-arguments DEBUG log regardless of which key carries it.
            arguments: json!({"count": 3, "language": support::SENTINEL_ADDRESS}),
            sentinels: vec![support::SENTINEL_ADDRESS.to_string()],
        },
        StdioProbe {
            tool: "reverse_geocode",
            // `latitude` is omitted so this call fails validation (missing
            // latitude) before any network access.
            arguments: json!({"longitude": support::SENTINEL_LONGITUDE}),
            sentinels: vec![support::SENTINEL_LONGITUDE.to_string()],
        },
    ]
}

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

/// One `tools/call` request per probe in [`stdio_probes`], starting at
/// JSON-RPC id 100 so it never collides with the fixed ids the two tests
/// below add around it.
fn probe_requests() -> Vec<Value> {
    stdio_probes()
        .iter()
        .enumerate()
        .map(|(i, probe)| {
            json!({
                "jsonrpc": "2.0",
                "id": 100 + i,
                "method": "tools/call",
                "params": {"name": probe.tool, "arguments": probe.arguments},
            })
        })
        .collect()
}

#[test]
fn stdout_carries_only_jsonrpc_at_trace_level() {
    let mut requests = vec![
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    ];
    requests.extend(probe_requests());
    requests.push(json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}));

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
    let requests_with_id = requests.iter().filter(|r| r.get("id").is_some()).count();
    assert_eq!(
        replies, requests_with_id,
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
/// (or higher) line, for any tool in [`stdio_probes`]. The failure path is
/// what is driven here, so each sentinel is present in the raw arguments
/// regardless of what geocode-mcp's own validation does with them.
#[test]
fn no_probe_sentinel_reaches_an_info_line_on_the_failure_path() {
    let probes = stdio_probes();
    let mut requests = vec![
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
    ];
    requests.extend(probe_requests());
    requests.push(json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}));

    let output = run_requests("trace", &requests);
    assert!(
        output.status.success(),
        "geocode-mcp must exit cleanly: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");

    for probe in &probes {
        for sentinel in &probe.sentinels {
            let mut saw_at_debug = false;
            for line in stderr.lines() {
                if !line.contains(sentinel.as_str()) {
                    continue;
                }
                let level = line_level(line);
                assert!(
                    matches!(level, Some("DEBUG") | Some("TRACE")),
                    "{}'s sentinel reached a line at level {level:?}, at or above INFO: \
                     {line:?}",
                    probe.tool
                );
                if level == Some("DEBUG") {
                    saw_at_debug = true;
                }
            }
            assert!(
                saw_at_debug,
                "{}'s sentinel {sentinel:?} must still be reachable at DEBUG, or this test \
                 cannot tell a real fix from a line that was simply deleted; stderr was: \
                 {stderr:?}",
                probe.tool
            );
        }
    }
}
