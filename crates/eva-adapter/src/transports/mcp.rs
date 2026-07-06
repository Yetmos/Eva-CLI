//! MCP transport envelope backed by eva-mcp allowlist checks.

use crate::manifest::AdapterHandle;
use crate::runtime::{AdapterInvocation, AdapterInvokeReport};
use eva_core::EvaError;
use eva_mcp::{InMemoryMcpClient, McpAllowlist};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP transport with tool, resource, and prompt allowlists";

pub fn invoke(
    handle: &AdapterHandle,
    invocation: AdapterInvocation,
) -> Result<AdapterInvokeReport, EvaError> {
    let tool = handle.mcp_tool_for(&invocation.capability).ok_or_else(|| {
        EvaError::unsupported("MCP adapter has no allowlisted tool for capability")
            .with_context("adapter_id", handle.id.as_str())
            .with_context("capability", invocation.capability.as_str())
    })?;
    let client = InMemoryMcpClient::new(
        handle.id.clone(),
        McpAllowlist::from_tools(handle.mcp_tools.iter().cloned())?,
    );
    let session_config = handle.mcp_session_config()?;
    let request_id = invocation.request_id.clone();
    let capability = invocation.capability.clone();
    let call = client.call_tool(invocation.request_id, tool, &invocation.input)?;
    Ok(AdapterInvokeReport {
        request_id,
        adapter_id: handle.id.clone(),
        transport: handle.transport,
        capability,
        status: "completed".to_owned(),
        output: call.output.as_text().unwrap_or_default().to_owned(),
        audit: vec![
            format!("adapter.invoked:{}", handle.id.as_str()),
            format!("mcp.tool.call:{tool}"),
            format!(
                "mcp.server_transport:{}",
                session_config.server_transport.as_str()
            ),
            format!("mcp.command:{}", session_config.process.command),
            "mcp.session_boundary:not_started".to_owned(),
        ],
    })
}
