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

/// Build the MCP server configuration.
///
/// The `instructions` blurb is emitted in the `initialize` response and is what
/// the daemon indexes as this server's model-facing, searchable description, so
/// it must lead with purpose and name the tools.
fn server_config() -> ServerConfig {
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

#[tokio::main]
async fn main() -> mcp_core::Result<()> {
    mcp_core::run_simple(server_config(), || async { Ok(GeocodeService::new()) }).await
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
}
