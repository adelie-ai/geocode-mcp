use thiserror::Error;

/// Geocode operation errors.
#[derive(Error, Debug)]
pub enum GeocodeError {
    /// Location not found
    #[error("Location not found: {0}")]
    LocationNotFound(String),

    /// API error (upstream HTTP error etc.)
    #[error("API error: {0}")]
    ApiError(String),

    /// Invalid parameters
    #[error("Invalid parameters: {0}")]
    InvalidParameters(String),
}

/// Invalid tool parameters (missing or malformed args).
#[derive(Error, Debug)]
pub enum McpError {
    /// Tool not found
    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    /// Invalid tool parameters
    #[error("Invalid tool parameters: {0}")]
    InvalidToolParameters(String),
}

/// Top-level error type wrapping all domain errors.
#[derive(Error, Debug)]
pub enum GeocodeMcpError {
    #[error(transparent)]
    Geocode(#[from] GeocodeError),
    #[error(transparent)]
    Mcp(#[from] McpError),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result type alias for convenience.
pub type Result<T> = std::result::Result<T, GeocodeMcpError>;
