use geocode_mcp::{build_service, server_config};

#[tokio::main]
async fn main() -> mcp_core::Result<()> {
    mcp_core::run_simple(server_config(), || async { Ok(build_service()) }).await
}
