#![deny(warnings)]

use crate::error::{McpError, Result};
use crate::operations::geocode;
use crate::operations::reverse_geocode;
use serde_json::Value;
use std::time::Duration;

/// Tool registry that manages all available tools
pub struct ToolRegistry {
    client: reqwest::Client,
}

impl ToolRegistry {
    /// Create a new tool registry
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
        }
    }

    /// Get all tools in MCP format
    pub fn list_tools(&self) -> Value {
        serde_json::json!([
            {
                "name": "geocode",
                "description": "Resolve a location name to geographic coordinates (latitude and longitude) using the Photon geocoding API (powered by OpenStreetMap). Returns up to 'count' matching locations with their coordinates, country, country code, region, and place type. Supports cities, addresses, and points of interest.",
                "inputSchema": {
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
                }
            },
            {
                "name": "reverse_geocode",
                "description": "Resolve geographic coordinates (latitude and longitude) to a location name and address using the Photon reverse geocoding API (powered by OpenStreetMap). Returns the nearest matching location with its name, country, region, and place type.",
                "inputSchema": {
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
                }
            }
        ])
    }

    /// Execute a tool call by name with given arguments
    pub async fn execute_tool(&self, tool_name: &str, arguments: &Value) -> Result<Value> {
        match tool_name {
            "geocode" => self.execute_geocode(arguments).await,
            "reverse_geocode" => self.execute_reverse_geocode(arguments).await,
            _ => Err(McpError::ToolNotFound(tool_name.to_string()).into()),
        }
    }

    async fn execute_geocode(&self, arguments: &Value) -> Result<Value> {
        let name = arguments
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                McpError::InvalidToolParameters("Missing required parameter: name".to_string())
            })?;

        if name.trim().is_empty() {
            return Err(McpError::InvalidToolParameters(
                "Parameter 'name' must not be empty or whitespace-only".to_string(),
            )
            .into());
        }

        let count = arguments.get("count").and_then(value_as_u64).unwrap_or(5) as u32;

        let language = arguments.get("language").and_then(|v| v.as_str());

        let result = geocode::geocode_location(&self.client, name, count, language).await?;

        Ok(mcp_tool_result_text(result))
    }

    async fn execute_reverse_geocode(&self, arguments: &Value) -> Result<Value> {
        let latitude = arguments
            .get("latitude")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| {
                McpError::InvalidToolParameters("Missing required parameter: latitude".to_string())
            })?;

        let longitude = arguments
            .get("longitude")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| {
                McpError::InvalidToolParameters("Missing required parameter: longitude".to_string())
            })?;

        let language = arguments.get("language").and_then(|v| v.as_str());

        let result =
            reverse_geocode::reverse_geocode_location(&self.client, latitude, longitude, language)
                .await?;

        Ok(mcp_tool_result_text(result))
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract a u64 from a JSON value, accepting both numbers and numeric strings.
fn value_as_u64(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse::<u64>().ok())
}

/// Wrap a JSON value in the MCP tool result content format as a `text` content block.
///
/// MCP spec content types are `text`, `image`, and `resource`; `json` is non-standard.
fn mcp_tool_result_text(value: Value) -> Value {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    serde_json::json!({
        "content": [
            {
                "type": "text",
                "text": text,
            }
        ]
    })
}
