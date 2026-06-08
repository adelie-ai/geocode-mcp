use geocode_mcp::error::{GeocodeError, GeocodeMcpError, McpError};
use geocode_mcp::operations::{geocode, reverse_geocode};
use mcp_core::{CallError, McpService, ServerConfig, ToolDef, ToolReply, async_trait};
use serde_json::{Value, json};
use std::time::Duration;

/// The geocoding service — holds the shared reqwest client.
struct GeocodeService {
    client: reqwest::Client,
}

impl GeocodeService {
    fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
        }
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
                "Resolve geographic coordinates (latitude and longitude) to a location name \
                 and address using the Photon reverse geocoding API (powered by OpenStreetMap). \
                 Returns the nearest matching location with its name, country, region, and \
                 place type.",
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

        let result = geocode::geocode_location(&self.client, name, count, language)
            .await
            .map_err(domain_err_to_call_error)?;

        ToolReply::json(&result).map_err(CallError::from)
    }

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

        let result =
            reverse_geocode::reverse_geocode_location(&self.client, latitude, longitude, language)
                .await
                .map_err(domain_err_to_call_error)?;

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

#[tokio::main]
async fn main() -> mcp_core::Result<()> {
    mcp_core::run_simple(
        ServerConfig::new("geocode-mcp", env!("CARGO_PKG_VERSION"))
            .without_websocket()
            .instructions(
                "Geocoding tools backed by the Photon API (OpenStreetMap): \
                 resolve place names to coordinates and back.",
            ),
        || async { Ok(GeocodeService::new()) },
    )
    .await
}
