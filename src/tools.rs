#![deny(warnings)]

use crate::error::{McpError, Result};
use crate::operations::geocode;
use serde_json::Value;

/// Tool registry that manages all available tools
pub struct ToolRegistry {
    client: reqwest::Client,
}

impl ToolRegistry {
    /// Create a new tool registry
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
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
                            "type": "number",
                            "description": "Maximum number of results to return. Range: 1-10 (default: 5)."
                        },
                        "language": {
                            "type": "string",
                            "description": "Language for result names (ISO 639-1 code). Default: 'en'. Example: 'de', 'fr', 'es'."
                        }
                    },
                    "required": ["name"]
                }
            }
        ])
    }

    /// Execute a tool call by name with given arguments
    pub async fn execute_tool(&self, tool_name: &str, arguments: &Value) -> Result<Value> {
        match tool_name {
            "geocode" => self.execute_geocode(arguments).await,
            _ => Err(McpError::ToolNotFound(tool_name.to_string()).into()),
        }
    }

    async fn execute_geocode(&self, arguments: &Value) -> Result<Value> {
        let name = arguments
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidToolParameters("Missing required parameter: name".to_string()))?;

        let count = arguments
            .get("count")
            .and_then(value_as_u64)
            .unwrap_or(5) as u32;

        let language = arguments
            .get("language")
            .and_then(|v| v.as_str());

        let result = geocode::geocode_location(&self.client, name, count, language).await?;

        Ok(mcp_tool_result_json(result))
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

/// Wrap a JSON value in the MCP tool result content format.
fn mcp_tool_result_json(value: Value) -> Value {
    serde_json::json!({
        "content": [
            {
                "type": "json",
                "value": value,
            }
        ]
    })
}
