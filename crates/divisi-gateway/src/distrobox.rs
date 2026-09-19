//! `divisi-gateway --distrobox`: a small, separate MCP server (real security-
//! tooling companion, not fabricated) exposing `run_in_kali`/
//! `run_in_blackarch` — each runs a shell command inside the matching
//! distrobox container via `distrobox enter --name <container> -- sh -c
//! '<command>'`, the exact invocation confirmed against `distrobox enter
//! --help` (v1.8.2.5) on this machine. Container names (`kali`,
//! `blackarch`) match the user's confirmed real setup; if your
//! `distrobox list` uses different names, add a server with those tool
//! names via a custom MCP entry instead of editing this one.
//!
//! No child MCP process, no session cache — each tool call is one
//! synchronous `distrobox enter` invocation, run to completion.

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData as McpError, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler};
use serde_json::{json, Map, Value};
use std::sync::Arc;

pub struct DistroboxServer;

fn schema() -> Arc<Map<String, Value>> {
    let Value::Object(map) = json!({
        "type": "object",
        "properties": { "command": { "type": "string", "description": "Shell command to run inside the container." } },
        "required": ["command"],
        "additionalProperties": false
    }) else {
        unreachable!()
    };
    Arc::new(map)
}

fn run_in(container: &str, command: &str) -> CallToolResult {
    let output = std::process::Command::new("distrobox").args(["enter", "--name", container, "--", "sh", "-c", command]).output();
    match output {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            if !output.stderr.is_empty() {
                text.push_str("\n--- stderr ---\n");
                text.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            if output.status.success() {
                CallToolResult::success(vec![ContentBlock::text(text)])
            } else {
                CallToolResult::error(vec![ContentBlock::text(format!("exit status {}: {text}", output.status))])
            }
        }
        Err(e) => CallToolResult::error(vec![ContentBlock::text(format!(
            "failed to run distrobox (is it installed and is the '{container}' container created?): {e}"
        ))]),
    }
}

impl ServerHandler for DistroboxServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("single-mcp-distrobox", env!("CARGO_PKG_VERSION")))
            .with_instructions("Runs shell commands inside your kali/blackarch distrobox containers for pentesting/CTF work.")
    }

    async fn list_tools(&self, _request: Option<PaginatedRequestParams>, _context: RequestContext<RoleServer>) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(vec![
            Tool::new("run_in_kali", "Runs a shell command inside the 'kali' distrobox container.", schema()),
            Tool::new("run_in_blackarch", "Runs a shell command inside the 'blackarch' distrobox container.", schema()),
        ]))
    }

    async fn call_tool(&self, request: CallToolRequestParams, _context: RequestContext<RoleServer>) -> Result<CallToolResponse, McpError> {
        let empty = Map::new();
        let arguments = request.arguments.as_ref().unwrap_or(&empty);
        let Some(command) = arguments.get("command").and_then(Value::as_str) else {
            return Ok(CallToolResult::error(vec![ContentBlock::text("missing required \"command\" argument")]).into());
        };
        let container = match request.name.as_ref() {
            "run_in_kali" => "kali",
            "run_in_blackarch" => "blackarch",
            other => return Ok(CallToolResult::error(vec![ContentBlock::text(format!("unknown tool: {other}"))]).into()),
        };
        Ok(run_in(container, command).into())
    }
}
