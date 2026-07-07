//! MCP transport backed by eva-mcp JSON-RPC allowlist checks.

use crate::manifest::AdapterHandle;
use crate::runtime::{AdapterInvocation, AdapterInvokeReport};
use eva_core::EvaError;
use eva_mcp::{McpAllowlist, McpJsonRpcClient, McpJsonRpcClientConfig};

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
    validate_input_size(handle, &invocation.input)?;
    let session_config = handle.mcp_session_config()?;
    let request_id = invocation.request_id.clone();
    let capability = invocation.capability.clone();
    let trace = invocation.trace_for_adapter(&handle.id);
    let client = McpJsonRpcClient::new(
        handle.id.clone(),
        McpAllowlist::from_tools(handle.mcp_tools.iter().cloned())?,
    )
    .with_config(
        McpJsonRpcClientConfig::new()
            .with_request_timeout_ms(timeout_ms(handle))
            .with_output_limit_bytes(output_limit_bytes(handle)),
    );
    let call = client.call_stdio(
        &session_config,
        invocation.request_id,
        tool,
        &invocation.input,
    )?;
    let output = call.output.as_text().unwrap_or_default().to_owned();
    let mut audit = vec![
        format!("adapter.invoked:{}", handle.id.as_str()),
        format!("mcp.tool.call:{tool}"),
        format!(
            "mcp.server_transport:{}",
            session_config.server_transport.as_str()
        ),
        format!("mcp.command:{}", session_config.process.command),
    ];
    audit.extend(call.audit);
    Ok(AdapterInvokeReport {
        request_id,
        adapter_id: handle.id.clone(),
        transport: handle.transport,
        capability,
        status: "completed".to_owned(),
        output,
        audit,
        trace,
    })
}

fn validate_input_size(handle: &AdapterHandle, input: &str) -> Result<(), EvaError> {
    if let Some(limit) = handle.max_prompt_bytes {
        if input.len() > limit {
            return Err(
                EvaError::conflict("MCP provider input exceeded prompt limit")
                    .with_context("adapter_id", handle.id.as_str())
                    .with_context("max_prompt_bytes", limit.to_string())
                    .with_context("actual_bytes", input.len().to_string()),
            );
        }
    }
    Ok(())
}

fn timeout_ms(handle: &AdapterHandle) -> u64 {
    handle.timeout_ms.unwrap_or(30_000)
}

fn output_limit_bytes(handle: &AdapterHandle) -> usize {
    handle
        .output_limit_bytes
        .or(handle.max_prompt_bytes)
        .unwrap_or(64 * 1024)
}
