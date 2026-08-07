// McpService implementation: wires the geocoding operations into the mcp-core
// dispatch loop and defines the server-level MCP configuration.

use crate::error::{GeocodeError, GeocodeMcpError, McpError};
use crate::operations::{geocode, reverse_geocode};
use mcp_core::telemetry::metrics::{self, Label};
use mcp_core::{CallError, McpService, ServerConfig, ToolDef, ToolReply, async_trait};
use serde_json::{Value, json};
use std::time::Duration;

/// The geocoding service - holds the shared `reqwest` client used for all
/// Photon API calls, and the base URL each tool sends its outbound request
/// to.
///
/// Implements [`McpService`] to expose the `geocode` and `reverse_geocode`
/// tools. Construct it with [`GeocodeService::new`] (or [`Default`]); it can be
/// hosted by the standalone binary or compiled directly into a client via
/// [`crate::build_service`].
pub struct GeocodeService {
    client: reqwest::Client,
    geocode_base_url: String,
    reverse_geocode_base_url: String,
}

impl GeocodeService {
    /// Construct a service with the built-in defaults: a shared `reqwest`
    /// client with a 10s request timeout and 5s connect timeout, talking to the
    /// public Photon endpoint (no API key required).
    pub fn new() -> Self {
        Self::with_base_urls(geocode::PHOTON_API_URL, reverse_geocode::PHOTON_REVERSE_URL)
    }

    /// Construct a service pointed at custom Photon endpoints.
    ///
    /// Why: a test needs to drive a real outbound request -- both
    /// outbound-request `debug!` sites, the response-parsing branches, and
    /// `record_upstream_failure` -- through a local mock instead of the
    /// live API. Production code always goes through [`GeocodeService::new`];
    /// this constructor exists for that test need.
    pub fn with_base_urls(
        geocode_base_url: impl Into<String>,
        reverse_geocode_base_url: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
            geocode_base_url: geocode_base_url.into(),
            reverse_geocode_base_url: reverse_geocode_base_url.into(),
        }
    }
}

impl Default for GeocodeService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl McpService for GeocodeService {
    fn tools(&self) -> Vec<ToolDef> {
        vec![
            ToolDef::new(
                "geocode",
                "Resolve a location name to geographic coordinates (latitude and longitude) \
                 using the Photon geocoding API (powered by OpenStreetMap). Returns up to \
                 'count' matching locations with their coordinates, country, country code, \
                 region, and place type. Supports cities, addresses, and points of interest.",
                json!({
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Location name to search for. Supports city names, addresses, and points of interest. Examples: 'London', '1600 Pennsylvania Avenue', 'Eiffel Tower'."
                        },
                        "count": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 10,
                            "default": 5,
                            "description": "Maximum number of results to return. Range: 1-10 (default: 5)."
                        },
                        "language": {
                            "type": "string",
                            "description": "Language for result names (ISO 639-1 code). Default: 'en'. Example: 'de', 'fr', 'es'."
                        }
                    },
                    "required": ["name"]
                }),
            ),
            ToolDef::new(
                "reverse_geocode",
                "Resolve geographic coordinates (latitude and longitude) into the nearest place: \
                 its name, country, country code, region, and place type. Use this to identify \
                 what is located at a GPS point or to turn a lat/long fix into a human-readable \
                 place name. Powered by the Photon reverse geocoding API (OpenStreetMap).",
                json!({
                    "type": "object",
                    "properties": {
                        "latitude": {
                            "type": "number",
                            "description": "Latitude in decimal degrees (e.g. 51.5074 for London)."
                        },
                        "longitude": {
                            "type": "number",
                            "description": "Longitude in decimal degrees (e.g. -0.1278 for London)."
                        },
                        "language": {
                            "type": "string",
                            "description": "Language for result names (ISO 639-1 code). Default: 'en'. Example: 'de', 'fr', 'es'."
                        }
                    },
                    "required": ["latitude", "longitude"]
                }),
            ),
        ]
    }

    async fn call_tool(&self, name: &str, args: &Value) -> Result<ToolReply, CallError> {
        match name {
            "geocode" => self.call_geocode(args).await,
            "reverse_geocode" => self.call_reverse_geocode(args).await,
            other => Err(CallError::tool(format!("tool not found: {other}"))),
        }
    }
}

impl GeocodeService {
    // `args` carries the place name and is skipped: a tool argument is
    // content, so it must never become a span field (D10). The span still
    // gives this handler's own work its own timing, nested under
    // mcp-core's `mcp.tools.call` span.
    #[tracing::instrument(skip(self, args))]
    async fn call_geocode(&self, args: &Value) -> Result<ToolReply, CallError> {
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| CallError::invalid_params("Missing required parameter: name"))?;

        if name.trim().is_empty() {
            return Err(CallError::invalid_params(
                "Parameter 'name' must not be empty or whitespace-only",
            ));
        }

        let count = args.get("count").and_then(value_as_u64).unwrap_or(5) as u32;
        let language = args.get("language").and_then(Value::as_str);

        let outcome = geocode::geocode_location_with_base(
            &self.client,
            &self.geocode_base_url,
            name,
            count,
            language,
        )
        .await;
        record_upstream_failure("geocode", &outcome);
        let result = outcome.map_err(domain_err_to_call_error)?;

        ToolReply::json(&result).map_err(CallError::from)
    }

    // `args` carries the coordinate pair and is skipped for the same reason
    // as in `call_geocode`.
    #[tracing::instrument(skip(self, args))]
    async fn call_reverse_geocode(&self, args: &Value) -> Result<ToolReply, CallError> {
        let latitude = args
            .get("latitude")
            .and_then(Value::as_f64)
            .ok_or_else(|| CallError::invalid_params("Missing required parameter: latitude"))?;

        let longitude = args
            .get("longitude")
            .and_then(Value::as_f64)
            .ok_or_else(|| CallError::invalid_params("Missing required parameter: longitude"))?;

        let language = args.get("language").and_then(Value::as_str);

        let outcome = reverse_geocode::reverse_geocode_location_with_base(
            &self.client,
            &self.reverse_geocode_base_url,
            latitude,
            longitude,
            language,
        )
        .await;
        record_upstream_failure("reverse_geocode", &outcome);
        let result = outcome.map_err(domain_err_to_call_error)?;

        ToolReply::json(&result).map_err(CallError::from)
    }
}

/// Map a domain error to a `CallError`.
///
/// - `McpError::InvalidToolParameters` / `GeocodeError::InvalidParameters` →
///   `CallError::InvalidParams` (JSON-RPC -32602, visible before the LLM sees it)
/// - Everything else (not found, API errors, network) →
///   `CallError::Tool` (surfaced as `isError: true` content so the LLM can react)
fn domain_err_to_call_error(e: GeocodeMcpError) -> CallError {
    match &e {
        GeocodeMcpError::Mcp(McpError::InvalidToolParameters(_)) => {
            CallError::invalid_params(e.to_string())
        }
        GeocodeMcpError::Geocode(GeocodeError::InvalidParameters(_)) => {
            CallError::invalid_params(e.to_string())
        }
        _ => CallError::tool(e.to_string()),
    }
}

/// Extract a u64 from a JSON value, accepting both numbers and numeric strings.
fn value_as_u64(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse::<u64>().ok())
}

/// Classify a lookup failure as an upstream fault worth counting, or `None`
/// for a business decline (no matching location) or a caller mistake (bad
/// parameters). Rule 8.2 keeps an operational decline out of a failure
/// counter: `geocode.upstream_failures` tracks a fault reaching outward, not
/// an ordinary "no results" answer or a caller's own bad input.
///
/// Exhaustive over [`GeocodeMcpError`], so a new variant forces this
/// classification to be revisited rather than silently landing as "not
/// counted".
fn upstream_failure_reason(err: &GeocodeMcpError) -> Option<&'static str> {
    match err {
        GeocodeMcpError::Geocode(GeocodeError::ApiError(_)) => Some("http_error"),
        GeocodeMcpError::Http(_) => Some("network"),
        GeocodeMcpError::Geocode(GeocodeError::LocationNotFound(_))
        | GeocodeMcpError::Geocode(GeocodeError::InvalidParameters(_))
        | GeocodeMcpError::Mcp(_)
        | GeocodeMcpError::Json(_)
        | GeocodeMcpError::Io(_) => None,
    }
}

/// Count an upstream-reaching failure against `geocode.upstream_failures`.
///
/// `tool` is always one of the two `&'static str` literals its two call
/// sites pass, so the label is bounded there rather than by anything a
/// caller supplies; `reason` is bounded the same way, by
/// [`upstream_failure_reason`]'s fixed set of return values. Neither label
/// is ever built from a place name or a coordinate.
fn record_upstream_failure(tool: &'static str, outcome: &Result<Value, GeocodeMcpError>) {
    if let Err(err) = outcome
        && let Some(reason) = upstream_failure_reason(err)
    {
        metrics::increment(
            "geocode.upstream_failures",
            &[Label::new("tool", tool), Label::new("reason", reason)],
        );
    }
}

/// Build the MCP server configuration.
///
/// The `instructions` blurb is emitted in the `initialize` response and is what
/// the daemon indexes as this server's model-facing, searchable description, so
/// it must lead with purpose and name the tools.
pub fn server_config() -> ServerConfig {
    ServerConfig::new("geocode-mcp", env!("CARGO_PKG_VERSION"))
        .without_websocket()
        .instructions(
            "Convert between place names and geographic coordinates. Reach for this \
             whenever you need the latitude and longitude of a city, street address, \
             landmark, or point of interest (use `geocode`), or need to turn a lat/long \
             pair back into a human-readable place name, country, and region (use \
             `reverse_geocode`) -- for example to supply coordinates to a weather, \
             mapping, or distance lookup, or to identify where a GPS fix is located. \
             Backed by the Photon API (OpenStreetMap) with global coverage and no API \
             key or configuration required; every result is structured (coordinates, \
             country, country code, region, and place type).",
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Look up a tool's description by name from the live service definition.
    fn tool_description(name: &str) -> String {
        GeocodeService::new()
            .tools()
            .into_iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("tool '{name}' not found in tools()"))
            .description
    }

    /// The server must advertise a non-empty, model-facing `instructions` blurb —
    /// the daemon uses it as this server's searchable description.
    #[test]
    fn server_config_sets_non_empty_instructions() {
        let instructions = server_config()
            .instructions
            .expect("server_config() must set instructions");
        assert!(
            !instructions.trim().is_empty(),
            "instructions must not be empty or whitespace-only"
        );
    }

    /// The instructions must name both tools and the core concepts a model would
    /// search on, so tool discovery routes location questions here.
    #[test]
    fn instructions_name_both_tools_and_core_concepts() {
        let instructions = server_config()
            .instructions
            .expect("server_config() must set instructions")
            .to_lowercase();
        for needle in ["geocode", "reverse_geocode", "coordinates", "latitude"] {
            assert!(
                instructions.contains(needle),
                "instructions should mention '{needle}', got: {instructions}"
            );
        }
    }

    /// `reverse_geocode` must describe itself in natural terms (coordinates, a
    /// GPS point, a place) and must not claim to return a full street address —
    /// the server only surfaces name, country, region, and place type.
    #[test]
    fn reverse_geocode_description_is_natural_and_honest() {
        let desc = tool_description("reverse_geocode").to_lowercase();
        for needle in ["coordinates", "latitude", "place", "gps"] {
            assert!(
                desc.contains(needle),
                "reverse_geocode description should mention '{needle}', got: {desc}"
            );
        }
        assert!(
            !desc.contains("address"),
            "reverse_geocode must not over-claim returning an 'address': {desc}"
        );
    }

    // ── Telemetry (mcp-core#40) ────────────────────────────────────────────

    /// The metrics registry [`mcp_core::telemetry::metrics`] records into is
    /// process-global, and `cargo test` runs a file's tests concurrently by
    /// default. This guards every test below that reads the registry, so a
    /// concurrent write from elsewhere in this module cannot inflate its
    /// before/after delta.
    static METRICS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_metrics() -> std::sync::MutexGuard<'static, ()> {
        METRICS_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A `reqwest::Error` built from a URL that fails to parse. This is a
    /// client-side failure with no network access at all -- exactly the
    /// shape of the `Http` variant a real connection failure or timeout
    /// would also produce, without this test reaching any service.
    fn synthetic_reqwest_error() -> reqwest::Error {
        reqwest::Client::new()
            .get("not a valid url")
            .build()
            .expect_err("a malformed url must fail to build, with no network access")
    }

    #[test]
    fn upstream_failure_reason_counts_http_error_and_network_faults() {
        assert_eq!(
            upstream_failure_reason(&GeocodeMcpError::Geocode(GeocodeError::ApiError(
                "x".into()
            ))),
            Some("http_error")
        );
        assert_eq!(
            upstream_failure_reason(&GeocodeMcpError::Http(synthetic_reqwest_error())),
            Some("network")
        );
    }

    /// A "no results" answer and bad caller input are declines, not a fault
    /// reaching outward -- rule 8.2 keeps an operational decline out of a
    /// failure counter.
    #[test]
    fn upstream_failure_reason_excludes_declines_and_caller_errors() {
        assert_eq!(
            upstream_failure_reason(&GeocodeMcpError::Geocode(GeocodeError::LocationNotFound(
                "x".into()
            ))),
            None
        );
        assert_eq!(
            upstream_failure_reason(&GeocodeMcpError::Geocode(GeocodeError::InvalidParameters(
                "x".into()
            ))),
            None
        );
        assert_eq!(
            upstream_failure_reason(&GeocodeMcpError::Mcp(McpError::InvalidToolParameters(
                "x".into()
            ))),
            None
        );
        let json_err = serde_json::from_str::<Value>("not json").unwrap_err();
        assert_eq!(
            upstream_failure_reason(&GeocodeMcpError::Json(json_err)),
            None
        );
        assert_eq!(
            upstream_failure_reason(&GeocodeMcpError::Io(std::io::Error::other("x"))),
            None
        );
    }

    #[test]
    fn record_upstream_failure_increments_only_for_counted_reasons() {
        let _guard = lock_metrics();
        let labels = [
            Label::new("tool", "geocode"),
            Label::new("reason", "http_error"),
        ];
        let before = counter_total("geocode.upstream_failures", &labels);

        let ok: Result<Value, GeocodeMcpError> = Ok(json!([]));
        record_upstream_failure("geocode", &ok);
        let not_found: Result<Value, GeocodeMcpError> = Err(GeocodeMcpError::Geocode(
            GeocodeError::LocationNotFound("x".into()),
        ));
        record_upstream_failure("geocode", &not_found);
        assert_eq!(
            counter_total("geocode.upstream_failures", &labels),
            before,
            "a successful call or a 'no results' answer must not move the counter"
        );

        let http_failed: Result<Value, GeocodeMcpError> =
            Err(GeocodeMcpError::Geocode(GeocodeError::ApiError("x".into())));
        record_upstream_failure("geocode", &http_failed);
        assert_eq!(
            counter_total("geocode.upstream_failures", &labels),
            before + 1,
            "an upstream HTTP error must increment the counter, labelled by tool and reason"
        );
    }

    fn counter_total(name: &str, labels: &[Label]) -> u64 {
        metrics::global()
            .snapshot()
            .counters
            .iter()
            .find(|counter| counter.name == name && same_labels(&counter.labels, labels))
            .map_or(0, |counter| counter.total)
    }

    fn same_labels(recorded: &[Label], wanted: &[Label]) -> bool {
        recorded.len() == wanted.len()
            && wanted.iter().all(|want| {
                recorded
                    .iter()
                    .any(|have| have.key() == want.key() && have.value() == want.value())
            })
    }
}
