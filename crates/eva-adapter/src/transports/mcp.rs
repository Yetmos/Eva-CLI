//! MCP transport backed by eva-mcp JSON-RPC allowlist checks.

use crate::manifest::AdapterHandle;
use crate::runtime::{AdapterInvocation, AdapterInvokeReport};
use crate::stream::{
    capture_provider_bytes, default_provider_artifact_root, provider_stream_audit,
    provider_stream_key, provider_stream_summary_json, ProviderStreamConfig,
};
use crate::supervisor::validate_credential_scope_for_provider;
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
    let credential_scope = validate_credential_scope_for_provider(
        invocation.credential_scope(),
        &handle.id,
        &invocation.request_id,
        &invocation.capability,
        false,
    )?
    .cloned();
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
    let output_stream = capture_provider_bytes(
        ProviderStreamConfig::new("result", output_limit_bytes(handle)).with_artifact(
            default_provider_artifact_root(&handle.source_path),
            provider_stream_key(
                "provider",
                handle.id.as_str(),
                request_id.as_str(),
                "mcp-result",
            ),
            "application/json",
        ),
        output.into_bytes(),
        1,
        false,
        &[],
    )?;
    let mut audit = vec![
        format!("adapter.invoked:{}", handle.id.as_str()),
        format!("mcp.tool.call:{tool}"),
        format!(
            "mcp.server_transport:{}",
            session_config.server_transport.as_str()
        ),
        format!("mcp.command:{}", session_config.process.command),
    ];
    if let Some(scope) = &credential_scope {
        audit.extend(scope.audit_entries());
    }
    audit.extend(call.audit);
    audit.extend(provider_stream_audit(&output_stream));
    Ok(AdapterInvokeReport {
        request_id,
        adapter_id: handle.id.clone(),
        transport: handle.transport,
        capability,
        status: "completed".to_owned(),
        output: format!(
            "{{\"transport\":\"mcp\",\"tool\":{},\"result\":{}}}",
            json_string(tool),
            provider_stream_summary_json(&output_stream)
        ),
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

fn json_string(value: &str) -> String {
    let mut escaped = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            value => escaped.push(value),
        }
    }
    escaped.push('"');
    escaped
}
