//! 将适配器清单转换为受控 MCP 工具调用。
//!
//! 工具名先经过显式允许列表，HTTP 模式还要求与当前提供者、请求和能力完全匹配的凭据
//! 作用域；这些检查均发生在启动进程或建立网络连接之前。协议响应大小与超时沿用适配器
//! 限额，鉴权头的原值不会进入审计输出。
//! MCP transport backed by eva-mcp JSON-RPC allowlist checks.

use crate::credential_vault::{
    default_credential_vault, minimal_process_env, sanitize_error_with_values,
    CredentialSessionLease, CredentialVault,
};
use crate::manifest::AdapterHandle;
use crate::process_backend::{OsProcessBackend, ProviderProcessHandle, ProviderProcessSpawner};
use crate::runtime::{AdapterInvocation, AdapterInvokeReport};
use crate::stream::{
    capture_provider_bytes, default_provider_artifact_root, provider_stream_audit,
    provider_stream_key, provider_stream_summary_json, ProviderStreamConfig,
};
use crate::supervisor::{
    validate_credential_scope_for_provider, McpHttpLifecycleHandle, McpHttpOperationPermit,
};
use eva_core::EvaError;
use eva_mcp::{
    McpAllowlist, McpJsonRpcCallReport, McpJsonRpcClient, McpJsonRpcClientConfig,
    McpServerTransport, McpStdioProcess, McpStreamableHttpConfig, McpStreamableHttpSession,
    McpTlsMaterial, McpTransportConfig,
};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

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

    fn terminate_gracefully(&mut self, timeout: Duration) -> Result<(), EvaError> {
        ProviderProcessHandle::terminate_gracefully(self, timeout).map(|_| ())
    }
}

/// 执行 `invoke` 对应的受控流程。
pub fn invoke(
    handle: &AdapterHandle,
    invocation: AdapterInvocation,
) -> Result<AdapterInvokeReport, EvaError> {
    let vault = default_credential_vault();
    invoke_with_spawner_and_vault(handle, invocation, None, vault.as_ref())
}

/// Invoke MCP while optionally supplying the runtime's central process
/// registrar. HTTP remains process-free; only MCP stdio consumes the hook.
pub fn invoke_with_spawner(
    handle: &AdapterHandle,
    invocation: AdapterInvocation,
    process_spawner: Option<&dyn ProviderProcessSpawner>,
) -> Result<AdapterInvokeReport, EvaError> {
    let vault = default_credential_vault();
    invoke_with_spawner_and_vault(handle, invocation, process_spawner, vault.as_ref())
}

/// Invoke MCP with an explicit credential authority.
pub fn invoke_with_spawner_and_vault(
    handle: &AdapterHandle,
    invocation: AdapterInvocation,
    process_spawner: Option<&dyn ProviderProcessSpawner>,
    vault: &dyn CredentialVault,
) -> Result<AdapterInvokeReport, EvaError> {
    invoke_with_supervisor_and_vault(handle, invocation, process_spawner, None, vault)
}

/// Invoke MCP with the daemon supervisor's shared Streamable HTTP lifecycle
/// authority. Streamable HTTP fails closed without that authority because a
/// call-local registry cannot retain failed remote cleanup for later drain.
pub(crate) fn invoke_with_supervisor_and_vault(
    handle: &AdapterHandle,
    invocation: AdapterInvocation,
    process_spawner: Option<&dyn ProviderProcessSpawner>,
    mcp_http_lifecycle: Option<&McpHttpLifecycleHandle>,
    vault: &dyn CredentialVault,
) -> Result<AdapterInvokeReport, EvaError> {
    let tool = handle.mcp_tool_for(&invocation.capability).ok_or_else(|| {
        EvaError::unsupported("MCP adapter has no allowlisted tool for capability")
            .with_context("adapter_id", handle.id.as_str())
            .with_context("capability", invocation.capability.as_str())
    })?;
    validate_input_size(handle, &invocation.input)?;
    let server_transport =
        McpServerTransport::parse(handle.mcp_server_transport.as_deref().unwrap_or("stdio"))?;
    let http_config = if server_transport.is_http() {
        match handle.mcp_transport_config()? {
            McpTransportConfig::StreamableHttp(config) => Some(config),
            McpTransportConfig::Stdio(_) => {
                return Err(EvaError::internal(
                    "MCP HTTP transport resolved to stdio configuration",
                )
                .with_context("adapter_id", handle.id.as_str()))
            }
        }
    } else {
        None
    };
    if server_transport.is_http() && mcp_http_lifecycle.is_none() {
        return Err(EvaError::unsupported(
            "MCP HTTP transport requires a supervisor-owned lifecycle authority",
        )
        .with_provider_code("mcp_http_lifecycle_authority_required"));
    }
    if http_config.as_ref().is_some_and(|config| {
        config
            .trust_roots
            .iter()
            .any(|reference| reference.starts_with("pem:"))
    }) {
        return Err(EvaError::permission_denied(
            "MCP indirect trust root material resolver is unavailable",
        )
        .with_provider_code("mcp_tls_indirect_material_unavailable"));
    }
    if server_transport == McpServerTransport::Stdio
        && handle
            .headers
            .values()
            .any(|value| value.strip_prefix("env:").is_some())
    {
        return Err(EvaError::unsupported(
            "MCP stdio transport does not support credential headers",
        )
        .with_context("adapter_id", handle.id.as_str()));
    }
    match (server_transport, &handle.provider.run_as) {
        (
            McpServerTransport::Http | McpServerTransport::StreamableHttp,
            eva_config::ProviderRunAsIdentity::Current,
        ) => {}
        (McpServerTransport::Http | McpServerTransport::StreamableHttp, run_as) => {
            return Err(EvaError::permission_denied(
                "process-free MCP HTTP transport cannot apply a run-as identity",
            )
            .with_context("run_as_kind", run_as.kind())
            .with_context("adapter_id", handle.id.as_str()));
        }
        (McpServerTransport::Stdio, run_as) => match process_spawner {
            Some(spawner) => spawner.validate_provider_run_as(run_as)?,
            None => OsProcessBackend::new().validate_run_as(run_as)?,
        },
    }
    let credential_scope = validate_credential_scope_for_provider(
        invocation.credential_scope(),
        &handle.id,
        &invocation.request_id,
        &invocation.capability,
        server_transport.is_http()
            || !handle.credential_env.is_empty()
            || !handle.provider.vault_secrets.is_empty()
            || handle
                .headers
                .values()
                .any(|value| value.strip_prefix("env:").is_some()),
    )?
    .cloned();
    let mut lazy_credential_env = handle
        .headers
        .values()
        .filter_map(|value| value.strip_prefix("env:"))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if let Some(client_auth) = http_config
        .as_ref()
        .and_then(|config| config.client_auth.as_ref())
    {
        lazy_credential_env.extend(
            [&client_auth.certificate_ref, &client_auth.private_key_ref]
                .into_iter()
                .filter_map(|reference| reference.strip_prefix("env:"))
                .map(str::to_owned),
        );
    }
    lazy_credential_env.sort();
    lazy_credential_env.dedup();
    let http_lifecycle = if server_transport.is_http() {
        Some(
            mcp_http_lifecycle
                .expect("HTTP transport lifecycle authority was checked before vault access"),
        )
    } else {
        None
    };
    let http_operation = http_lifecycle
        .map(McpHttpLifecycleHandle::begin_operation)
        .transpose()?;
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
    let credential_lease = CredentialSessionLease::open_with_lazy_env_retaining_owner(
        vault,
        credential_scope.as_ref(),
        &handle.provider.vault_secrets,
        &handle.credential_env,
        &lazy_credential_env,
    );
    let mut credential_lease = match credential_lease {
        Ok(credentials) => Some(credentials),
        Err(failure) => {
            let (mut error, credentials) = failure.into_parts();
            if let Some(credentials) = credentials {
                let release = if server_transport.is_http() {
                    http_lifecycle
                        .expect("HTTP transport has a lifecycle authority")
                        .release_detached_credentials(
                            http_operation
                                .as_ref()
                                .expect("HTTP transport holds an operation permit"),
                            credentials,
                        )
                        .map(|_| ())
                } else {
                    let mut credentials = credentials;
                    credentials.release()
                };
                if let Err(release_error) = release {
                    error =
                        error.with_context("credential_release_error", release_error.to_string());
                }
            }
            return Err(error);
        }
    };
    let mut child_env = BTreeMap::new();
    if server_transport == McpServerTransport::Stdio {
        credential_lease
            .as_mut()
            .expect("credential lease is caller-owned before transport dispatch")
            .inject_env(&mut child_env);
        if let Some(scope) = &credential_scope {
            scope.apply_env(&mut child_env);
        }
    }
    let mut sensitive_values = Vec::new();
    if let Some(scope) = &credential_scope {
        sensitive_values.extend(scope.redaction_values());
    }
    let mut transport_audit = vec![format!(
        "mcp.server_transport:{}",
        server_transport.canonical_str()
    )];
    let call_result = match server_transport {
        McpServerTransport::Stdio => {
            let session_config = handle.mcp_session_config()?;
            transport_audit.push(format!("mcp.command:{}", session_config.process.command));
            let mut command = Command::new(&session_config.process.command);
            command
                .args(&session_config.process.args)
                .env_clear()
                .envs(minimal_process_env(&child_env))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let process = match process_spawner {
                Some(spawner) => spawner
                    .spawn_provider_as(command, &handle.provider.run_as)
                    .map_err(|error| map_mcp_spawn_error(error, handle)),
                None => OsProcessBackend::new()
                    .spawn_provider_as(command, &handle.provider.run_as)
                    .map_err(|error| map_mcp_spawn_error(error, handle)),
            };
            let process = process?;
            client.call_stdio_with_process(
                &session_config,
                Box::new(process),
                invocation.request_id,
                tool,
                &invocation.input,
            )
        }
        McpServerTransport::Http | McpServerTransport::StreamableHttp => (|| {
            let config = http_config.as_ref().ok_or_else(|| {
                EvaError::internal("MCP HTTP configuration was not retained")
                    .with_context("adapter_id", handle.id.as_str())
            })?;
            let credentials = credential_lease
                .as_mut()
                .expect("credential lease is caller-owned before HTTP registration");
            let mut header_plan = mcp_http_headers(handle, credentials)?;
            if let Some(scope) = &credential_scope {
                scope.apply_headers(&mut header_plan.headers);
            }
            sensitive_values.extend(header_plan.sensitive_values.clone());
            transport_audit.push(format!("mcp.endpoint_origin:{}", config.endpoint_origin()?));
            transport_audit.extend(header_plan.audit);
            let tls_material = mcp_tls_material(handle, config, credentials)?;
            sensitive_values.extend(credentials.redaction_values());
            let credentials = credential_lease
                .take()
                .expect("credential lease transfers exactly once to HTTP lifecycle ownership");
            let lifecycle = http_lifecycle.expect("HTTP transport has a lifecycle authority");
            let operation = http_operation
                .as_ref()
                .expect("HTTP transport holds an operation permit");
            call_http_with_lifecycle(
                handle,
                &client,
                lifecycle,
                operation,
                config,
                McpHttpCall {
                    request_id: invocation.request_id,
                    tool,
                    input: &invocation.input,
                    headers: header_plan.headers,
                    tls_material,
                    credentials,
                },
            )
        })(),
    };
    if let Some(credentials) = credential_lease.as_ref() {
        sensitive_values.extend(credentials.redaction_values());
    }
    let error_redactions = sensitive_values.clone();
    child_env.clear();
    let (call, credential_audit) = if let Some(credentials) = credential_lease {
        let release = if server_transport.is_http() {
            http_lifecycle
                .expect("HTTP transport has a lifecycle authority")
                .release_detached_credentials(
                    http_operation
                        .as_ref()
                        .expect("HTTP transport holds an operation permit"),
                    credentials,
                )
        } else {
            let mut credentials = credentials;
            credentials.release().map(|()| credentials.audit_entries())
        };
        match (call_result, release) {
            (Err(error), Ok(_)) => {
                return Err(sanitize_error_with_values(error, &error_redactions));
            }
            (Err(error), Err(release_error)) => {
                return Err(sanitize_error_with_values(
                    error.with_context("credential_release_error", release_error.to_string()),
                    &error_redactions,
                ));
            }
            (Ok(_), Err(error)) => {
                return Err(sanitize_error_with_values(error, &error_redactions));
            }
            (Ok(call), Ok(audit)) => (call, audit),
        }
    } else {
        let call =
            call_result.map_err(|error| sanitize_error_with_values(error, &error_redactions))?;
        (call, Vec::new())
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
    audit.extend(credential_audit);
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

struct McpHttpCall<'a> {
    request_id: eva_core::RequestId,
    tool: &'a str,
    input: &'a str,
    headers: BTreeMap<String, String>,
    tls_material: McpTlsMaterial,
    credentials: CredentialSessionLease,
}

fn call_http_with_lifecycle(
    handle: &AdapterHandle,
    client: &McpJsonRpcClient,
    lifecycle: &McpHttpLifecycleHandle,
    operation: &McpHttpOperationPermit,
    config: &McpStreamableHttpConfig,
    call: McpHttpCall<'_>,
) -> Result<McpJsonRpcCallReport, EvaError> {
    let McpHttpCall {
        request_id,
        tool,
        input,
        headers,
        tls_material,
        credentials,
    } = call;
    let session = McpStreamableHttpSession::new_with_tls(
        config.clone(),
        headers,
        tls_material,
        Duration::from_millis(timeout_ms(handle)),
        output_limit_bytes(handle),
    );
    let session = match session {
        Ok(session) => session,
        Err(error) => {
            let release = lifecycle.release_detached_credentials(operation, credentials);
            return Err(match release {
                Ok(_) => error,
                Err(release_error) => {
                    error.with_context("credential_release_error", release_error.to_string())
                }
            });
        }
    };
    let registered =
        lifecycle.register_starting_session(operation, handle.id.clone(), session, credentials)?;
    let registry_session_id = registered.session_id.clone();
    let call = lifecycle.call_tool(
        operation,
        &registry_session_id,
        client,
        request_id,
        tool,
        input,
    );
    let shutdown = lifecycle.shutdown_session(operation, &registry_session_id);
    match (call, shutdown) {
        (Ok(mut report), Ok((shutdown, credential_audit))) => {
            report.audit.extend(registered.audit);
            report.audit.extend(shutdown.audit);
            report.audit.extend(credential_audit);
            report
                .audit
                .push("mcp.http_registry:supervisor_owned_call".to_owned());
            Ok(report)
        }
        (Err(error), Ok(_)) => Err(error),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error
            .with_context("mcp_call_completed", "true")
            .with_context("mcp_session_cleanup", "failed")),
        (Err(error), Err(cleanup_error)) => Err(error
            .with_context("mcp_session_cleanup", "failed")
            .with_context(
                "mcp_session_cleanup_code",
                cleanup_error
                    .provider_code()
                    .map(|code| code.as_str())
                    .unwrap_or_else(|| cleanup_error.kind().as_str()),
            )),
    }
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
#[derive(Clone, PartialEq, Eq)]
struct McpHeaderPlan {
    /// 记录 `headers` 字段对应的值。
    headers: BTreeMap<String, String>,
    /// 记录 `audit` 字段对应的值。
    audit: Vec<String>,
    /// 记录 `sensitive_values` 字段对应的值。
    sensitive_values: Vec<String>,
}

/// 执行 `mcp_http_headers` 对应的处理逻辑。
fn mcp_http_headers(
    handle: &AdapterHandle,
    credentials: &mut CredentialSessionLease,
) -> Result<McpHeaderPlan, EvaError> {
    let mut headers = BTreeMap::new();
    let mut audit = Vec::new();
    let mut sensitive_values = Vec::new();
    for (name, value) in &handle.headers {
        if let Some(env_name) = value.strip_prefix("env:") {
            let env_value = credentials.resolve_env(env_name).map_err(|_| {
                EvaError::permission_denied("MCP HTTP credential environment variable is missing")
                    .with_provider_code("missing_credential")
                    .with_context("adapter_id", handle.id.as_str())
                    .with_context("env", env_name)
            })?;
            headers.insert(name.clone(), env_value.clone());
            if !env_value.is_empty() {
                sensitive_values.push(env_value.clone());
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

fn mcp_tls_material(
    handle: &AdapterHandle,
    config: &McpStreamableHttpConfig,
    credentials: &mut CredentialSessionLease,
) -> Result<McpTlsMaterial, EvaError> {
    let mut material = McpTlsMaterial::new();
    if let Some(project_root) = &handle.project_root {
        material = material.with_project_root(project_root);
    }
    if let Some(client_auth) = &config.client_auth {
        let certificate = credentials
            .resolve_reference(&client_auth.certificate_ref, &handle.provider.vault_secrets)?;
        let private_key = credentials
            .resolve_reference(&client_auth.private_key_ref, &handle.provider.vault_secrets)?;
        material = material.with_client_auth(certificate.into_bytes(), private_key.into_bytes())?;
    }
    Ok(material)
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
    use crate::credential_vault::{CredentialSession, SecretValue};
    use crate::supervisor::ProviderCredentialScope;
    use eva_config::{AdapterTransport, ProviderVaultSecretRef};
    use eva_core::{CapabilityName, ErrorKind, RequestId};
    use eva_mcp::{McpClientAuthConfig, McpRedirectPolicy};
    use std::fmt;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    const TEST_CA_PEM: &str = include_str!("../../../eva-mcp/testdata/tls/ca.pem");
    const TEST_CLIENT_PEM: &str = include_str!("../../../eva-mcp/testdata/tls/client.pem");
    const TEST_CLIENT_KEY: &str = include_str!("../../../eva-mcp/testdata/tls/client.key");

    /// 验证 `http_mcp_requires_provider_credential_scope_before_rpc` 场景下的预期行为。
    #[test]
    fn http_mcp_requires_provider_credential_scope_before_rpc() {
        let handle = http_mcp_handle(BTreeMap::new());
        let vault = crate::credential_vault::MemoryCredentialVault::new();
        let error = invoke_http_for_test(
            &handle,
            AdapterInvocation::new(
                RequestId::parse("req-mcp-missing-scope").unwrap(),
                CapabilityName::parse("github.issue.list").unwrap(),
            ),
            &vault,
        )
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(error.message().contains("credential session"));
    }

    #[test]
    fn direct_http_invocation_requires_lifecycle_authority_before_vault_open() {
        let handle = http_mcp_handle(BTreeMap::new());
        let opens = Arc::new(AtomicUsize::new(0));
        let vault = TestCredentialVault {
            values: BTreeMap::new(),
            opens: Arc::clone(&opens),
            releases: Arc::new(AtomicUsize::new(0)),
        };

        let error = invoke_with_spawner_and_vault(
            &handle,
            AdapterInvocation::new(
                RequestId::parse("req-mcp-missing-lifecycle").unwrap(),
                CapabilityName::parse("github.issue.list").unwrap(),
            ),
            None,
            &vault,
        )
        .unwrap_err();

        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("mcp_http_lifecycle_authority_required")
        );
        assert_eq!(opens.load(Ordering::SeqCst), 0);
    }

    /// 验证 `http_mcp_missing_auth_env_returns_policy_error` 场景下的预期行为。
    #[test]
    fn http_mcp_missing_auth_env_returns_policy_error() {
        let env_name = "EVA_TEST_MCP_HTTP_MISSING_AUTH";
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

        let vault = crate::credential_vault::MemoryCredentialVault::new();
        let error = invoke_http_for_test(
            &handle,
            AdapterInvocation::new(request_id, capability).with_credential_scope(scope),
            &vault,
        )
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("missing_credential")
        );
    }

    #[test]
    fn drained_http_lifecycle_rejects_before_opening_the_vault() {
        let lifecycle = McpHttpLifecycleHandle::new();
        lifecycle
            .drain_all_until(Instant::now() + Duration::from_secs(1))
            .unwrap();
        let handle = http_mcp_handle(BTreeMap::from([(
            "Authorization".to_owned(),
            "env:EVA_TEST_DRAINED_MCP_TOKEN".to_owned(),
        )]));
        let request_id = RequestId::parse("req-mcp-drained-before-vault").unwrap();
        let capability = CapabilityName::parse("github.issue.list").unwrap();
        let scope = ProviderCredentialScope::new_for_session(
            "session-mcp-drained-before-vault",
            handle.id.clone(),
            request_id.clone(),
            capability.clone(),
        );
        let opens = Arc::new(AtomicUsize::new(0));
        let vault = TestCredentialVault {
            values: BTreeMap::from([(
                "EVA_TEST_DRAINED_MCP_TOKEN".to_owned(),
                "must-not-be-opened".to_owned(),
            )]),
            opens: Arc::clone(&opens),
            releases: Arc::new(AtomicUsize::new(0)),
        };

        let error = invoke_with_supervisor_and_vault(
            &handle,
            AdapterInvocation::new(request_id, capability).with_credential_scope(scope),
            None,
            Some(&lifecycle),
            &vault,
        )
        .unwrap_err();

        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("mcp_http_registry_admission_closed")
        );
        assert_eq!(opens.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn partial_vault_open_failure_remains_owned_until_drain() {
        let lifecycle = McpHttpLifecycleHandle::new();
        let mut handle = http_mcp_handle(BTreeMap::new());
        handle.credential_env = vec!["EVA_TEST_FAILING_FETCH".to_owned()];
        let request_id = RequestId::parse("req-mcp-partial-vault-open").unwrap();
        let capability = CapabilityName::parse("github.issue.list").unwrap();
        let scope = ProviderCredentialScope::new_for_session(
            "session-mcp-partial-vault-open",
            handle.id.clone(),
            request_id.clone(),
            capability.clone(),
        );
        let release_attempts = Arc::new(AtomicUsize::new(0));
        let vault = FailingFetchCredentialVault {
            release_attempts: Arc::clone(&release_attempts),
            release_failures_remaining: Arc::new(AtomicUsize::new(1)),
        };

        let error = invoke_with_supervisor_and_vault(
            &handle,
            AdapterInvocation::new(request_id, capability).with_credential_scope(scope),
            None,
            Some(&lifecycle),
            &vault,
        )
        .unwrap_err();

        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("missing_credential")
        );
        assert_eq!(release_attempts.load(Ordering::SeqCst), 1);
        let report = lifecycle
            .drain_all_until(Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert!(report.complete);
        assert_eq!(release_attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn http_mtls_resolves_env_and_vault_material_per_call() {
        let project_root = tls_project_root("material");
        for (name, certificate_ref, private_key_ref, vault_refs, values) in [
            (
                "env",
                "env:MCP_CLIENT_CERT",
                "env:MCP_CLIENT_KEY",
                Vec::new(),
                BTreeMap::from([
                    ("MCP_CLIENT_CERT".to_owned(), TEST_CLIENT_PEM.to_owned()),
                    ("MCP_CLIENT_KEY".to_owned(), TEST_CLIENT_KEY.to_owned()),
                ]),
            ),
            (
                "vault",
                "vault://tests/mcp/client-cert",
                "vault://tests/mcp/client-key",
                vec![
                    ProviderVaultSecretRef {
                        env: "MCP_CLIENT_CERT".to_owned(),
                        secret_ref: "vault://tests/mcp/client-cert".to_owned(),
                    },
                    ProviderVaultSecretRef {
                        env: "MCP_CLIENT_KEY".to_owned(),
                        secret_ref: "vault://tests/mcp/client-key".to_owned(),
                    },
                ],
                BTreeMap::from([
                    (
                        "vault://tests/mcp/client-cert".to_owned(),
                        TEST_CLIENT_PEM.to_owned(),
                    ),
                    (
                        "vault://tests/mcp/client-key".to_owned(),
                        TEST_CLIENT_KEY.to_owned(),
                    ),
                ]),
            ),
        ] {
            let port = unused_local_port();
            let mut handle = tls_mcp_handle(
                project_root.clone(),
                port,
                certificate_ref,
                private_key_ref,
                ["file:certs/ca.pem"],
            );
            handle.provider.vault_secrets = vault_refs;
            let opens = Arc::new(AtomicUsize::new(0));
            let releases = Arc::new(AtomicUsize::new(0));
            let vault = TestCredentialVault {
                values,
                opens: opens.clone(),
                releases: releases.clone(),
            };
            let request_id = RequestId::parse(&format!("req-mcp-mtls-{name}")).unwrap();
            let capability = CapabilityName::parse("github.issue.list").unwrap();
            let scope = ProviderCredentialScope::new_for_session(
                format!("session-mcp-mtls-{name}"),
                handle.id.clone(),
                request_id.clone(),
                capability.clone(),
            );

            let error = invoke_http_for_test(
                &handle,
                AdapterInvocation::new(request_id, capability).with_credential_scope(scope),
                &vault,
            )
            .unwrap_err();

            assert!(
                matches!(
                    error.provider_code().map(|code| code.as_str()),
                    Some("mcp_http_connect_failed" | "mcp_http_connect_timeout")
                ),
                "{name}: {error:?}"
            );
            let debug = format!("{error:?}");
            assert!(!debug.contains("-----BEGIN CERTIFICATE-----"));
            assert!(!debug.contains("-----BEGIN PRIVATE KEY-----"));
            assert_eq!(opens.load(Ordering::SeqCst), 1, "{name}");
            assert_eq!(releases.load(Ordering::SeqCst), 1, "{name}");
        }
        std::fs::remove_dir_all(project_root).unwrap();
    }

    #[test]
    fn http_mtls_missing_and_ambiguous_references_fail_closed_and_release() {
        let project_root = tls_project_root("references");

        let handle = tls_mcp_handle(
            project_root.clone(),
            unused_local_port(),
            "env:MCP_CLIENT_CERT",
            "env:MCP_CLIENT_KEY",
            ["file:certs/ca.pem"],
        );
        let opens = Arc::new(AtomicUsize::new(0));
        let releases = Arc::new(AtomicUsize::new(0));
        let vault = TestCredentialVault {
            values: BTreeMap::new(),
            opens: opens.clone(),
            releases: releases.clone(),
        };
        let error = invoke_tls_for_test(&handle, "missing", &vault).unwrap_err();
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("missing_credential")
        );
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        assert_eq!(releases.load(Ordering::SeqCst), 1);

        let certificate_ref = "vault://tests/mcp/shared-cert";
        let private_key_ref = "vault://tests/mcp/client-key";
        let mut handle = tls_mcp_handle(
            project_root.clone(),
            unused_local_port(),
            certificate_ref,
            private_key_ref,
            ["file:certs/ca.pem"],
        );
        handle.provider.vault_secrets = vec![
            ProviderVaultSecretRef {
                env: "MCP_CLIENT_CERT_A".to_owned(),
                secret_ref: certificate_ref.to_owned(),
            },
            ProviderVaultSecretRef {
                env: "MCP_CLIENT_CERT_B".to_owned(),
                secret_ref: certificate_ref.to_owned(),
            },
            ProviderVaultSecretRef {
                env: "MCP_CLIENT_KEY".to_owned(),
                secret_ref: private_key_ref.to_owned(),
            },
        ];
        let opens = Arc::new(AtomicUsize::new(0));
        let releases = Arc::new(AtomicUsize::new(0));
        let vault = TestCredentialVault {
            values: BTreeMap::from([
                (certificate_ref.to_owned(), TEST_CLIENT_PEM.to_owned()),
                (private_key_ref.to_owned(), TEST_CLIENT_KEY.to_owned()),
            ]),
            opens: opens.clone(),
            releases: releases.clone(),
        };
        let error = invoke_tls_for_test(&handle, "ambiguous", &vault).unwrap_err();
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("credential_ref_ambiguous")
        );
        let debug = format!("{error:?}");
        assert!(!debug.contains(certificate_ref));
        assert!(!debug.contains("-----BEGIN CERTIFICATE-----"));
        assert!(!debug.contains("-----BEGIN PRIVATE KEY-----"));
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        assert_eq!(releases.load(Ordering::SeqCst), 1);

        std::fs::remove_dir_all(project_root).unwrap();
    }

    #[test]
    fn http_indirect_root_without_runtime_authority_fails_before_vault_open() {
        let project_root = tls_project_root("indirect");
        let handle = tls_mcp_handle(
            project_root.clone(),
            unused_local_port(),
            "env:MCP_CLIENT_CERT",
            "env:MCP_CLIENT_KEY",
            ["pem:sha256:test-root"],
        );
        let opens = Arc::new(AtomicUsize::new(0));
        let vault = TestCredentialVault {
            values: BTreeMap::new(),
            opens: opens.clone(),
            releases: Arc::new(AtomicUsize::new(0)),
        };

        let error = invoke_http_for_test(
            &handle,
            AdapterInvocation::new(
                RequestId::parse("req-mcp-indirect-root").unwrap(),
                CapabilityName::parse("github.issue.list").unwrap(),
            ),
            &vault,
        )
        .unwrap_err();

        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("mcp_tls_indirect_material_unavailable")
        );
        assert_eq!(opens.load(Ordering::SeqCst), 0);
        std::fs::remove_dir_all(project_root).unwrap();
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
            project_root: None,
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
            mcp_http_config: None,
            mcp_http_config_invalid: false,
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

    fn tls_mcp_handle(
        project_root: PathBuf,
        port: u16,
        certificate_ref: &str,
        private_key_ref: &str,
        trust_roots: impl IntoIterator<Item = &'static str>,
    ) -> AdapterHandle {
        let endpoint = format!("https://127.0.0.1:{port}/mcp");
        let origin = format!("https://127.0.0.1:{port}");
        let config = McpStreamableHttpConfig::from_parts(
            endpoint.clone(),
            trust_roots,
            Some(McpClientAuthConfig::new(certificate_ref, private_key_ref).unwrap()),
            McpRedirectPolicy::Deny,
            [origin],
        )
        .unwrap();
        let mut handle = http_mcp_handle(BTreeMap::new());
        handle.project_root = Some(project_root);
        handle.endpoint = Some(endpoint);
        handle.mcp_server_transport = Some("streamable_http".to_owned());
        handle.mcp_http_config = Some(config);
        handle
    }

    fn invoke_tls_for_test(
        handle: &AdapterHandle,
        name: &str,
        vault: &dyn CredentialVault,
    ) -> Result<AdapterInvokeReport, EvaError> {
        let request_id = RequestId::parse(&format!("req-mcp-mtls-{name}")).unwrap();
        let capability = CapabilityName::parse("github.issue.list").unwrap();
        let scope = ProviderCredentialScope::new_for_session(
            format!("session-mcp-mtls-{name}"),
            handle.id.clone(),
            request_id.clone(),
            capability.clone(),
        );
        invoke_http_for_test(
            handle,
            AdapterInvocation::new(request_id, capability).with_credential_scope(scope),
            vault,
        )
    }

    fn invoke_http_for_test(
        handle: &AdapterHandle,
        invocation: AdapterInvocation,
        vault: &dyn CredentialVault,
    ) -> Result<AdapterInvokeReport, EvaError> {
        let lifecycle = McpHttpLifecycleHandle::new();
        invoke_with_supervisor_and_vault(handle, invocation, None, Some(&lifecycle), vault)
    }

    fn tls_project_root(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "eva-adapter-mcp-tls-{name}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("certs")).unwrap();
        std::fs::write(root.join("certs/ca.pem"), TEST_CA_PEM).unwrap();
        std::fs::canonicalize(root).unwrap()
    }

    fn unused_local_port() -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    struct FailingFetchCredentialVault {
        release_attempts: Arc<AtomicUsize>,
        release_failures_remaining: Arc<AtomicUsize>,
    }

    impl fmt::Debug for FailingFetchCredentialVault {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("FailingFetchCredentialVault([REDACTED])")
        }
    }

    impl CredentialVault for FailingFetchCredentialVault {
        fn open_session(
            &self,
            _scope: &ProviderCredentialScope,
        ) -> Result<Box<dyn CredentialSession>, EvaError> {
            Ok(Box::new(FailingFetchCredentialSession {
                release_attempts: Arc::clone(&self.release_attempts),
                release_failures_remaining: Arc::clone(&self.release_failures_remaining),
                released: false,
            }))
        }
    }

    struct FailingFetchCredentialSession {
        release_attempts: Arc<AtomicUsize>,
        release_failures_remaining: Arc<AtomicUsize>,
        released: bool,
    }

    impl fmt::Debug for FailingFetchCredentialSession {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("FailingFetchCredentialSession")
                .field("released", &self.released)
                .finish()
        }
    }

    impl CredentialSession for FailingFetchCredentialSession {
        fn fetch(&mut self, _secret_ref: &str) -> Result<SecretValue, EvaError> {
            Err(EvaError::not_found("test credential fetch failed"))
        }

        fn release(&mut self) -> Result<(), EvaError> {
            if self.released {
                return Ok(());
            }
            self.release_attempts.fetch_add(1, Ordering::SeqCst);
            if self
                .release_failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(EvaError::unavailable("test credential release failed"));
            }
            self.released = true;
            Ok(())
        }
    }

    struct TestCredentialVault {
        values: BTreeMap<String, String>,
        opens: Arc<AtomicUsize>,
        releases: Arc<AtomicUsize>,
    }

    impl fmt::Debug for TestCredentialVault {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("TestCredentialVault")
                .field("secret_count", &self.values.len())
                .finish()
        }
    }

    impl CredentialVault for TestCredentialVault {
        fn open_session(
            &self,
            _scope: &ProviderCredentialScope,
        ) -> Result<Box<dyn CredentialSession>, EvaError> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(TestCredentialSession {
                values: self.values.clone(),
                releases: self.releases.clone(),
                released: false,
            }))
        }
    }

    struct TestCredentialSession {
        values: BTreeMap<String, String>,
        releases: Arc<AtomicUsize>,
        released: bool,
    }

    impl fmt::Debug for TestCredentialSession {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("TestCredentialSession")
                .field("secret_count", &self.values.len())
                .field("released", &self.released)
                .finish()
        }
    }

    impl CredentialSession for TestCredentialSession {
        fn fetch(&mut self, secret_ref: &str) -> Result<SecretValue, EvaError> {
            if self.released {
                return Err(EvaError::conflict("test credential session was released"));
            }
            self.values
                .get(secret_ref)
                .cloned()
                .map(SecretValue::new)
                .ok_or_else(|| {
                    EvaError::not_found("test credential material is unavailable")
                        .with_context("secret_ref", secret_ref)
                })
        }

        fn release(&mut self) -> Result<(), EvaError> {
            if !self.released {
                self.released = true;
                self.releases.fetch_add(1, Ordering::SeqCst);
                for value in self.values.values_mut() {
                    value.clear();
                }
                self.values.clear();
            }
            Ok(())
        }
    }
}
