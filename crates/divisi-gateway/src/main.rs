//! `divisi-gateway`: a dynamic MCP gateway (spec: "divisi-gateway inside single").
//!
//! Instead of registering 50+ MCP servers into an agent's native config
//! (burning context on every session), an agent registers only this one
//! binary. It exposes `list_available_mcp_tools`/`invoke_mcp` and lazily
//! proxies to whichever real server, from divisi's `mcp.toml` registry,
//! a call actually needs — see `gateway.rs` for the mechanics.

mod distrobox;
mod gateway;

use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    divisi_core::env::adopt_legacy_env();
    if std::env::args().any(|a| a == "--distrobox") {
        let service = distrobox::DistroboxServer.serve(rmcp::transport::io::stdio()).await?;
        service.waiting().await?;
        return Ok(());
    }
    let service = gateway::Gateway::new().serve(rmcp::transport::io::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
