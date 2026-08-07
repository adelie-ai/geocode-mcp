//! Shared sentinel probe table for geocode-mcp's telemetry content tests
//! (mcp-core#40, lesson 8: table-driven over the whole tool list, not one
//! tool).
//!
//! `tests/telemetry_stdio.rs` (console text) and
//! `tests/telemetry_span_fields.rs` (in-process span fields) both iterate
//! [`tool_probes`] rather than hard-coding one tool each. A coverage test in
//! `tests/telemetry_span_fields.rs` compares this table against the
//! service's live `tools()` list, so a tool that ships without an entry
//! here fails that test instead of silently going unaudited.
#![allow(dead_code)]

use serde_json::{Value, json};

/// A place name a caller might supply.
pub const SENTINEL_ADDRESS: &str = "MARKER-9f3d1c2a-sentinel-address";

/// A coordinate a caller might supply.
pub const SENTINEL_LATITUDE: f64 = 12.345678;
pub const SENTINEL_LONGITUDE: f64 = -98.765432;

/// One tool's sentinel-bearing failure-path probe.
pub struct ToolProbe {
    /// The MCP tool name, as advertised by `GeocodeService::tools()`.
    pub tool: &'static str,
    /// The span name `#[tracing::instrument]` gives this tool's handler.
    pub handler_span: &'static str,
    /// Arguments that fail geocode-mcp's own validation (a required field is
    /// missing) before any network access, while still carrying every
    /// sentinel below somewhere in the JSON payload -- mcp-core logs the
    /// whole payload at DEBUG regardless of what the handler does with it.
    pub arguments: Value,
    /// Substrings that must never reach a span field or an INFO-or-louder
    /// event, and must be reachable at DEBUG (the positive control).
    pub sentinels: Vec<String>,
}

/// One probe per tool this server advertises. Extend this whenever a tool
/// is added -- `tool_probe_table_covers_every_advertised_tool` in
/// `tests/telemetry_span_fields.rs` fails the build otherwise.
pub fn tool_probes() -> Vec<ToolProbe> {
    vec![
        ToolProbe {
            tool: "geocode",
            handler_span: "call_geocode",
            // `name` is omitted so this call fails validation (missing name)
            // before any network access; the sentinel travels through
            // `language` instead, which proves the same property since
            // `#[instrument(skip(self, args))]` skips the whole `arguments`
            // object, not individual keys within it.
            arguments: json!({"count": 3, "language": SENTINEL_ADDRESS}),
            sentinels: vec![SENTINEL_ADDRESS.to_string()],
        },
        ToolProbe {
            tool: "reverse_geocode",
            handler_span: "call_reverse_geocode",
            // `latitude` is omitted so this call fails validation (missing
            // latitude) before any network access.
            arguments: json!({"longitude": SENTINEL_LONGITUDE}),
            sentinels: vec![SENTINEL_LONGITUDE.to_string()],
        },
    ]
}
