//! 将适配器清单转换为受控 MCP 工具调用。
//!
//! 工具名先经过显式允许列表，HTTP 模式还要求与当前提供者、请求和能力完全匹配的凭据
//! 作用域；这些检查均发生在启动进程或建立网络连接之前。协议响应大小与超时沿用适配器
//! 限额，鉴权头的原值不会进入审计输出。
//! MCP transport backed by eva-mcp JSON-RPC allowlist checks.

use crate::manifest::AdapterHandle;
use crate::process_backend::{OsProcessBackend, ProviderProcessHandle, ProviderProcessSpawner};
use crate::runtime::{AdapterInvocation, AdapterInvokeReport};
use crate::stream::{
    capture_provider_bytes, default_provider_artifact_root, provider_stream_audit,
    provider_stream_key, provider_stream_summary_json, ProviderStreamConfig,
};
use crate::supervisor::validate_credential_scope_for_provider;
use eva_core::EvaError;
use eva_mcp::{
    McpAllowlist, McpJsonRpcClient, McpJsonRpcClientConfig, McpServerTransport, McpStdioProcess,
};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP transport with tool, resource, and prompt allowlists";

impl McpStdioProcess for ProviderProcessHandle {
    fn process_id(&self) -> u32 {
        self.pid()
    }

    fn take_stdin(&mut self) -> Option<Box<dyn Write + Send>> {
        ProviderProcessHandle::take_stdin(self).map(|pipe| Box::new(pipe) as _)
    }

    fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>> {
        ProviderProcessHandle::take_stdout(self).map(|pipe| Box::new(pipe) as _)
    }

    fn terminate(&mut self) -> Result<(), EvaError> {
        ProviderProcessHandle::terminate(self)
    }
}

/// 执行 `invoke` 对应的受控流程。
pub fn invoke(
    handle: &AdapterHandle,
    invocation: AdapterInvocation,
) -> Result<AdapterInvokeReport, EvaError> {
    invoke_with_spawner(handle, invocation, None)
}

/// Invoke MCP while optionally supplying the runtime's central process
/// registrar. HTTP remains process-free; only MCP stdio consumes the hook.
pub fn invoke_with_spawner(
    handle: &AdapterHandle,
    invocation: AdapterInvocation,
    process_spawner: Option<&dyn ProviderProcessSpawner>,
) -> Result<AdapterInvokeReport, EvaError> {
    let tool = handle.mcp_tool_for(&invocation.capability).ok_or_else(|| {
        EvaError::unsupported("MCP adapter has no allowlisted tool for capability")
            .with_context("adapter_id", handle.id.as_str())
            .with_context("capability", invocation.capability.as_str())
    })?;
    validate_input_size(handle, &invocation.input)?;
    let server_transport =
        McpServerTransport::parse(handle.mcp_server_transport.as_deref().unwrap_or("stdio"))?;
    let credential_scope = validate_credential_scope_for_provider(
        invocation.credential_scope(),
        &handle.id,
        &invocation.request_id,
        &invocation.capability,
        server_transport == McpServerTransport::Http,
    )?
    .cloned();
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
    let mut sensitive_values = Vec::new();
    let mut transport_audit = vec![format!(
        "mcp.server_transport:{}",
        server_transport.as_str()
    )];
    let call = match server_transport {
        McpServerTransport::Stdio => {
            let session_config = handle.mcp_session_config()?;
            transport_audit.push(format!("mcp.command:{}", session_config.process.command));
            let mut command = Command::new(&session_config.process.command);
            command
                .args(&session_config.process.args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let process = match process_spawner {
                Some(spawner) => spawner
                    .spawn_provider(command)
                    .map_err(|error| map_mcp_spawn_error(error, handle))?,
                None => OsProcessBackend::new()
                    .spawn_provider(command)
                    .map_err(|error| map_mcp_spawn_error(error, handle))?,
            };
            client.call_stdio_with_process(
                &session_config,
                Box::new(process),
                invocation.request_id,
                tool,
                &invocation.input,
            )?
        }
        McpServerTransport::Http => {
            let endpoint = handle.endpoint.as_deref().ok_or_else(|| {
                EvaError::invalid_argument("MCP HTTP adapter is missing endpoint")
                    .with_context("adapter_id", handle.id.as_str())
            })?;
            let mut header_plan = mcp_http_headers(handle)?;
            if let Some(scope) = &credential_scope {
                scope.apply_headers(&mut header_plan.headers);
                sensitive_values.extend(scope.redaction_values());
            }
            sensitive_values.extend(header_plan.sensitive_values.clone());
            transport_audit.push(format!("mcp.endpoint:{}", endpoint));
            transport_audit.extend(header_plan.audit);
            client.call_http(
                endpoint,
                header_plan.headers,
                invocation.request_id,
                tool,
                &invocation.input,
            )?
        }
    };
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
        &sensitive_values,
    )?;
    let mut audit = vec![
        format!("adapter.invoked:{}", handle.id.as_str()),
        format!("mcp.tool.call:{tool}"),
    ];
    audit.extend(transport_audit);
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

/// Preserve the historical MCP startup message while retaining the backend's
/// stable error kind, provider code, and non-sensitive context.
fn map_mcp_spawn_error(error: EvaError, handle: &AdapterHandle) -> EvaError {
    if error.message() != "failed to spawn provider process boundary" {
        return error
            .with_context("adapter_id", handle.id.as_str())
            .with_context("command", handle.mcp_command.as_deref().unwrap_or(""));
    }
    let mut mapped = EvaError::new(error.kind(), "failed to start MCP stdio server")
        .with_retryable(error.is_retryable())
        .with_error_context(error.context().clone());
    if let Some(code) = error.provider_code() {
        mapped = mapped.with_provider_code(code.as_str());
    }
    mapped
        .with_context("adapter_id", handle.id.as_str())
        .with_context("command", handle.mcp_command.as_deref().unwrap_or(""))
}

/// 校验 `validate_input_size` 对应的约束，不满足时返回明确错误。
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

/// 表示 `McpHeaderPlan` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
struct McpHeaderPlan {
    /// 记录 `headers` 字段对应的值。
    headers: BTreeMap<String, String>,
    /// 记录 `audit` 字段对应的值。
    audit: Vec<String>,
    /// 记录 `sensitive_values` 字段对应的值。
    sensitive_values: Vec<String>,
}

/// 执行 `mcp_http_headers` 对应的处理逻辑。
fn mcp_http_headers(handle: &AdapterHandle) -> Result<McpHeaderPlan, EvaError> {
    let mut headers = BTreeMap::new();
    let mut audit = Vec::new();
    let mut sensitive_values = Vec::new();
    for (name, value) in &handle.headers {
        if let Some(env_name) = value.strip_prefix("env:") {
            let env_value = std::env::var(env_name).map_err(|_| {
                EvaError::permission_denied("MCP HTTP credential environment variable is missing")
                    .with_provider_code("missing_credential")
                    .with_context("adapter_id", handle.id.as_str())
                    .with_context("env", env_name)
            })?;
            headers.insert(name.clone(), env_value.clone());
            if !env_value.is_empty() {
                sensitive_values.push(env_value);
            }
            audit.push(format!(
                "mcp.credential_header:{name}:env:{env_name}:redacted"
            ));
        } else {
            headers.insert(name.clone(), value.clone());
            audit.push(format!("mcp.http.header:{name}:literal"));
        }
    }
    Ok(McpHeaderPlan {
        headers,
        audit,
        sensitive_values,
    })
}

/// 执行 `timeout_ms` 对应的处理逻辑。
fn timeout_ms(handle: &AdapterHandle) -> u64 {
    handle.timeout_ms.unwrap_or(30_000)
}

/// 执行 `output_limit_bytes` 对应的处理逻辑。
fn output_limit_bytes(handle: &AdapterHandle) -> usize {
    handle
        .output_limit_bytes
        .or(handle.max_prompt_bytes)
        .unwrap_or(64 * 1024)
}

/// 执行 `json_string` 对应的处理逻辑。
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

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::ProviderCredentialScope;
    use eva_config::AdapterTransport;
    use eva_core::{CapabilityName, ErrorKind, RequestId};

    /// 验证 `http_mcp_requires_provider_credential_scope_before_rpc` 场景下的预期行为。
    #[test]
    fn http_mcp_requires_provider_credential_scope_before_rpc() {
        let handle = http_mcp_handle(BTreeMap::new());
        let error = invoke(
            &handle,
            AdapterInvocation::new(
                RequestId::parse("req-mcp-missing-scope").unwrap(),
                CapabilityName::parse("github.issue.list").unwrap(),
            ),
        )
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(error.message().contains("credential session"));
    }

    /// 验证 `http_mcp_missing_auth_env_returns_policy_error` 场景下的预期行为。
    #[test]
    fn http_mcp_missing_auth_env_returns_policy_error() {
        let env_name = "EVA_TEST_MCP_HTTP_MISSING_AUTH";
        std::env::remove_var(env_name);
        let handle = http_mcp_handle(BTreeMap::from([(
            "Authorization".to_owned(),
            format!("env:{env_name}"),
        )]));
        let request_id = RequestId::parse("req-mcp-missing-auth").unwrap();
        let capability = CapabilityName::parse("github.issue.list").unwrap();
        let scope = ProviderCredentialScope::new_for_session(
            "session-mcp-auth",
            handle.id.clone(),
            request_id.clone(),
            capability.clone(),
        );

        let error = invoke(
            &handle,
            AdapterInvocation::new(request_id, capability).with_credential_scope(scope),
        )
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("missing_credential")
        );
    }

    /// 执行 `http_mcp_handle` 对应的处理逻辑。
    fn http_mcp_handle(headers: BTreeMap<String, String>) -> AdapterHandle {
        AdapterHandle {
            id: eva_core::AdapterId::parse("mcp-http-test").unwrap(),
            name: "MCP HTTP Test".to_owned(),
            version: "1.0.0".to_owned(),
            enabled: true,
            transport: AdapterTransport::Mcp,
            capabilities: vec![CapabilityName::parse("github.issue.list").unwrap()],
            source_path: "test".to_owned(),
            command: None,
            args: Vec::new(),
            endpoint: Some("http://127.0.0.1:1/mcp".to_owned()),
            method: None,
            credential_env: Vec::new(),
            provider: eva_config::ProviderConfig::default(),
            timeout_ms: Some(1_000),
            max_concurrency: None,
            output_limit_bytes: Some(4096),
            max_prompt_bytes: Some(4096),
            rate_limit: None,
            circuit_breaker: None,
            headers,
            mcp_server_transport: Some("http".to_owned()),
            mcp_command: None,
            mcp_args: Vec::new(),
            mcp_tools: vec!["list_issues".to_owned()],
            skill_id: None,
            skill_kind: None,
            skill_runtime_gate: None,
            skill_path: None,
            skill_entry_type: None,
            skill_runner_command: None,
            skill_runner_args: Vec::new(),
            skill_artifact_root: None,
            skill_input_schema: None,
            hardware_logical_name: None,
            hardware_device_class: None,
            hardware_driver_id: None,
            hardware_driver_kind: None,
            bindings: Vec::new(),
        }
    }
}
