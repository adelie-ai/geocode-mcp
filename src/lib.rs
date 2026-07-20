pub mod error;
pub mod operations;
pub mod service;

pub use service::{GeocodeService, server_config};

/// Construct the geocode service with built-in defaults (Photon, no API key), for in-process (compiled-in) hosting.
///
/// Why: a client can host geocode-mcp in-process without launching the CLI. The
/// service is built with the same zero-config defaults as the standalone binary,
/// so the in-process and standalone hosting paths cannot drift.
pub fn build_service() -> GeocodeService {
    GeocodeService::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcp_core::McpService;

    #[test]
    fn build_service_exposes_tools() {
        let svc = build_service();
        assert!(
            !svc.tools().is_empty(),
            "geocode build_service() must expose at least one tool"
        );
    }
}
