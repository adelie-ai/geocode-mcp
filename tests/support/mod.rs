//! Shared sentinel probe table for geocode-mcp's telemetry content tests
//! (mcp-core#40, lesson 8: table-driven over the whole tool list, not one
//! tool).
//!
//! Each probe carries *valid* arguments, so a call built from it reaches
//! the live body of `geocode_location_with_base` /
//! `reverse_geocode_location_with_base` -- not just geocode-mcp's own
//! parameter validation. [`mock_service`] points a [`GeocodeService`] at a
//! local `httpmock` server instead of the live Photon API, using the same
//! `_with_base` injection point `tests/mcp_stdio_suite.rs`'s existing
//! httpmock unit tests already rely on.
//!
//! `tests/telemetry_span_fields.rs` (in-process span fields) iterates
//! [`tool_probes`] against the mock. A coverage test there compares the
//! table against the service's live `tools()` list, so a tool that ships
//! without an entry here fails that test instead of silently going
//! unaudited.
//!
//! `tests/telemetry_stdio.rs` spawns the real binary as a separate OS
//! process, which cannot be redirected to this in-test mock server, so it
//! does not use this table -- see that file's own header comment.
#![allow(dead_code)]

use geocode_mcp::GeocodeService;
use httpmock::Method::GET;
use httpmock::MockServer;
use serde_json::{Value, json};

/// A place name a caller might supply.
pub const SENTINEL_ADDRESS: &str = "MARKER-9f3d1c2a-sentinel-address";

/// A coordinate a caller might supply.
pub const SENTINEL_LATITUDE: f64 = 12.345678;
pub const SENTINEL_LONGITUDE: f64 = -98.765432;

/// A minimal, valid Photon `features` response: one match. `PhotonResponse`
/// is the same shape for both `/api/` (geocode) and `/reverse`
/// (reverse_geocode), so one body serves both.
pub const SUCCESS_BODY: &str = r#"{
    "features": [{
        "properties": {
            "name": "Testville",
            "country": "Testland",
            "countrycode": "TL",
            "state": "Test Region",
            "type": "city"
        },
        "geometry": {"coordinates": [1.0, 2.0]}
    }]
}"#;

/// A valid Photon response with no matches. `GeocodeError::LocationNotFound`'s
/// `Display` embeds the caller's own place name or coordinates, so this is
/// the scenario a real upstream response most naturally carries content in
/// (mcp-core#40 lesson 9) -- and, per rule 8.2, a decline rather than a
/// fault: `upstream_failure_reason` must return `None` for it.
pub const NO_RESULTS_BODY: &str = r#"{"features":[]}"#;

/// One tool's sentinel-bearing probe, driven through a live outbound call.
pub struct ToolProbe {
    /// The MCP tool name, as advertised by `GeocodeService::tools()`.
    pub tool: &'static str,
    /// The span name `#[tracing::instrument]` gives this tool's handler.
    pub handler_span: &'static str,
    /// The path this tool's outbound request hits, matching the real
    /// Photon API's shape ("/api/" or "/reverse").
    pub mock_path: &'static str,
    /// Valid arguments -- unlike a validation-rejected request, these reach
    /// `geocode_location_with_base` / `reverse_geocode_location_with_base`.
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
            mock_path: "/api/",
            arguments: json!({"name": SENTINEL_ADDRESS, "count": 1}),
            sentinels: vec![SENTINEL_ADDRESS.to_string()],
        },
        ToolProbe {
            tool: "reverse_geocode",
            handler_span: "call_reverse_geocode",
            mock_path: "/reverse",
            arguments: json!({"latitude": SENTINEL_LATITUDE, "longitude": SENTINEL_LONGITUDE}),
            sentinels: vec![
                SENTINEL_LATITUDE.to_string(),
                SENTINEL_LONGITUDE.to_string(),
            ],
        },
    ]
}

/// Start a local mock server that answers `mock_path` with `status` and
/// `body`, and a [`GeocodeService`] pointed at it for both endpoints (only
/// the one under test is ever actually hit). Must run inside a Tokio
/// runtime; the returned `MockServer` has to be held for as long as the
/// service is used -- dropping it stops the mock.
pub async fn mock_service(
    mock_path: &'static str,
    status: u16,
    body: &'static str,
) -> (MockServer, GeocodeService) {
    let server = MockServer::start_async().await;
    server
        .mock_async(move |when, then| {
            when.method(GET).path(mock_path);
            then.status(status)
                .header("content-type", "application/json")
                .body(body);
        })
        .await;
    let service = GeocodeService::with_base_urls(
        format!("http://{}/api/", server.address()),
        format!("http://{}/reverse", server.address()),
    );
    (server, service)
}
