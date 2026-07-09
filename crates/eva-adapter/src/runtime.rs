//! Authorized Adapter runtime probes and controlled invocation envelopes.

use crate::registry::AdapterRegistry;
use crate::router::{AdapterRouteRequest, AdapterRouter};
use crate::supervisor::{
    InMemoryProviderSupervisor, ProviderCredentialScope, ProviderExecutionOutcome,
    ProviderExecutionRequest, ProviderSupervisor,
};
use crate::transports;
use eva_config::{AdapterTransport, ProjectConfig};
use eva_core::{AdapterId, CapabilityName, EvaError, RequestId};
use eva_observability::{SpanId, TraceFields};
use eva_policy::{HighRiskAction, PolicyDomainSet, RuntimePolicyGate, RuntimePolicyRequest};
use eva_storage::{FileSystemProviderProcessTable, ProviderProcessSnapshot};
use std::cell::RefCell;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "authorized transport execution with timeout and audit";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterInvocation {
    pub request_id: RequestId,
    pub capability: CapabilityName,
    pub provider: Option<AdapterId>,
    pub input: String,
    credential_scope: Option<ProviderCredentialScope>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterProbeReport {
    pub adapter_id: AdapterId,
    pub transport: AdapterTransport,
    pub status: String,
    pub capabilities: Vec<CapabilityName>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterInvokeReport {
    pub request_id: RequestId,
    pub adapter_id: AdapterId,
    pub transport: AdapterTransport,
    pub capability: CapabilityName,
    pub status: String,
    pub output: String,
    pub audit: Vec<String>,
    pub trace: TraceFields,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterRuntime {
    registry: AdapterRegistry,
    router: AdapterRouter,
    supervisor: RefCell<InMemoryProviderSupervisor>,
    policy_gate: RuntimePolicyGate,
}

impl AdapterInvocation {
    pub fn new(request_id: RequestId, capability: CapabilityName) -> Self {
        Self {
            request_id,
            capability,
            provider: None,
            input: String::new(),
            credential_scope: None,
        }
    }

    pub fn with_provider(mut self, provider: AdapterId) -> Self {
        self.provider = Some(provider);
        self
    }

    pub fn with_input(mut self, input: impl Into<String>) -> Self {
        self.input = input.into();
        self
    }

    pub(crate) fn with_credential_scope(mut self, scope: ProviderCredentialScope) -> Self {
        self.credential_scope = Some(scope);
        self
    }

    pub fn credential_scope(&self) -> Option<&ProviderCredentialScope> {
        self.credential_scope.as_ref()
    }

    pub fn trace_for_adapter(&self, adapter_id: &AdapterId) -> TraceFields {
        TraceFields::default()
            .with_request_id(self.request_id.clone())
            .with_adapter_id(adapter_id.clone())
            .with_capability(self.capability.clone())
            .with_provider(adapter_id.as_str())
            .with_span_id(
                SpanId::parse("adapter.invoke")
                    .expect("static adapter span identifiers use the observability character set"),
            )
    }
}

impl AdapterRuntime {
    pub fn from_registry(registry: AdapterRegistry) -> Self {
        Self::from_registry_with_policy(
            registry,
            RuntimePolicyGate::new(PolicyDomainSet::default()),
        )
    }

    fn from_registry_with_policy(
        registry: AdapterRegistry,
        policy_gate: RuntimePolicyGate,
    ) -> Self {
        Self::from_registry_with_policy_and_supervisor(
            registry,
            policy_gate,
            InMemoryProviderSupervisor::new(),
        )
    }

    fn from_registry_with_policy_and_supervisor(
        registry: AdapterRegistry,
        policy_gate: RuntimePolicyGate,
        supervisor: InMemoryProviderSupervisor,
    ) -> Self {
        let router = AdapterRouter::new(registry.clone());
        Self {
            registry,
            router,
            supervisor: RefCell::new(supervisor),
            policy_gate,
        }
    }

    pub fn from_project(project: &ProjectConfig) -> Result<Self, EvaError> {
        let registry = AdapterRegistry::from_project(project)?;
        let policy_gate = RuntimePolicyGate::from_project(project)?;
        Ok(Self::from_registry_with_policy(registry, policy_gate))
    }

    pub fn from_project_with_provider_process_table(
        project: &ProjectConfig,
        process_table: FileSystemProviderProcessTable,
    ) -> Result<Self, EvaError> {
        let registry = AdapterRegistry::from_project(project)?;
        let policy_gate = RuntimePolicyGate::from_project(project)?;
        Ok(Self::from_registry_with_policy_and_supervisor(
            registry,
            policy_gate,
            InMemoryProviderSupervisor::with_process_table(process_table),
        ))
    }

    pub fn from_registry_with_provider_process_table(
        registry: AdapterRegistry,
        process_table: FileSystemProviderProcessTable,
    ) -> Self {
        Self::from_registry_with_policy_and_supervisor(
            registry,
            RuntimePolicyGate::new(PolicyDomainSet::default()),
            InMemoryProviderSupervisor::with_process_table(process_table),
        )
    }

    pub fn registry(&self) -> &AdapterRegistry {
        &self.registry
    }

    pub fn router(&self) -> &AdapterRouter {
        &self.router
    }

    pub fn list(&self) -> Vec<&crate::manifest::AdapterHandle> {
        self.registry.list()
    }

    pub fn provider_processes(&self) -> Result<Vec<ProviderProcessSnapshot>, EvaError> {
        self.supervisor.borrow().processes()
    }

    pub fn probe_adapter(&self, adapter_id: &AdapterId) -> Result<AdapterProbeReport, EvaError> {
        let handle = self.registry.get(adapter_id).ok_or_else(|| {
            EvaError::not_found("Adapter provider does not exist")
                .with_context("adapter_id", adapter_id.as_str())
        })?;
        Ok(AdapterProbeReport {
            adapter_id: handle.id.clone(),
            transport: handle.transport,
            status: handle.health().as_str().to_owned(),
            capabilities: handle.capabilities.clone(),
            detail: if handle.enabled {
                "authorized handle is registered; probe has no external side effects".to_owned()
            } else {
                "adapter manifest is disabled".to_owned()
            },
        })
    }

    pub fn probe_capability(
        &self,
        capability: CapabilityName,
        provider: Option<AdapterId>,
    ) -> Result<AdapterProbeReport, EvaError> {
        let mut request = AdapterRouteRequest::new(capability);
        if let Some(provider) = provider {
            request = request.with_provider(provider);
        }
        let route = self.router.route(&request)?;
        self.probe_adapter(&route.handle.id)
    }

    pub fn invoke(&self, invocation: AdapterInvocation) -> Result<AdapterInvokeReport, EvaError> {
        if invocation.credential_scope().is_some() {
            return Err(EvaError::permission_denied(
                "caller supplied provider credential scope is not allowed",
            ));
        }
        let mut request = AdapterRouteRequest::new(invocation.capability.clone());
        if let Some(provider) = invocation.provider.clone() {
            request = request.with_provider(provider);
        }
        let route = self.router.route(&request)?;
        let handle = route.handle;

        if should_supervise(handle.transport) {
            return self.invoke_supervised(handle, invocation);
        }
        dispatch_transport(&handle, invocation)
    }

    fn invoke_supervised(
        &self,
        handle: crate::manifest::AdapterHandle,
        invocation: AdapterInvocation,
    ) -> Result<AdapterInvokeReport, EvaError> {
        let execution_request = ProviderExecutionRequest::from_handle(&handle, &invocation)
            .with_retry_backoff_ms(
                self.policy_gate
                    .adapter_retry_backoff_ms(&invocation.capability),
            );
        let slot = self.supervisor.borrow_mut().acquire(execution_request)?;
        let credential_scope =
            ProviderCredentialScope::from_slot(&slot, invocation.capability.clone());
        let policy_decision = self.policy_gate.decide(
            RuntimePolicyRequest::new(HighRiskAction::ProviderCredentialSession)
                .with_adapter(handle.id.clone())
                .with_provider(slot.adapter_id.clone())
                .with_capability(invocation.capability.clone()),
        );
        let policy_audit = policy_decision.audit.clone();
        if !policy_decision.allowed {
            self.supervisor.borrow_mut().complete(
                &slot,
                ProviderExecutionOutcome {
                    health: "failed".to_owned(),
                    last_error: Some(policy_decision.reason.clone()),
                },
            )?;
            let error = policy_decision
                .ensure_allowed()
                .expect_err("denied provider credential session returns an error");
            return Err(error);
        }
        let result =
            dispatch_transport(&handle, invocation.with_credential_scope(credential_scope));
        match result {
            Ok(mut report) => {
                let snapshot = self
                    .supervisor
                    .borrow_mut()
                    .complete(&slot, ProviderExecutionOutcome::completed(&report.status))?;
                report.audit.extend(policy_audit);
                append_supervisor_audit(&mut report.audit, &snapshot);
                Ok(report)
            }
            Err(error) => {
                self.supervisor
                    .borrow_mut()
                    .complete(&slot, ProviderExecutionOutcome::failed(&error))?;
                Err(error)
            }
        }
    }
}

fn dispatch_transport(
    handle: &crate::manifest::AdapterHandle,
    invocation: AdapterInvocation,
) -> Result<AdapterInvokeReport, EvaError> {
    match handle.transport {
        AdapterTransport::Mcp => transports::mcp::invoke(handle, invocation),
        AdapterTransport::Skill => transports::skill::invoke(handle, invocation),
        AdapterTransport::Builtin
        | AdapterTransport::LuaCapability
        | AdapterTransport::Eventbus => transports::builtin::invoke(handle, invocation),
        AdapterTransport::Hardware => transports::hardware::invoke(handle, invocation),
        AdapterTransport::Stdio => transports::stdio::invoke(handle, invocation),
        AdapterTransport::Http => transports::http::invoke(handle, invocation),
    }
}

fn should_supervise(transport: AdapterTransport) -> bool {
    matches!(
        transport,
        AdapterTransport::Mcp
            | AdapterTransport::Skill
            | AdapterTransport::Stdio
            | AdapterTransport::Http
    )
}

fn append_supervisor_audit(audit: &mut Vec<String>, snapshot: &ProviderProcessSnapshot) {
    audit.push(format!("provider.session:{}", snapshot.session_id));
    audit.push(format!("provider.process:{}", snapshot.provider_process_id));
    audit.push(format!(
        "provider.manifest_digest:{}",
        snapshot.manifest_digest
    ));
    audit.push(format!("provider.health:{}", snapshot.health));
    audit.push("provider.slot:released".to_owned());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{AdapterCircuitBreaker, AdapterHandle};
    use crate::registry::AdapterRegistry;
    use crate::supervisor::{
        ProviderCredentialScope, PROVIDER_SESSION_ID_HEADER, PROVIDER_SESSION_TOKEN_ENV,
        PROVIDER_SESSION_TOKEN_HEADER,
    };
    use eva_config::load_project_config;
    use eva_config::AdapterTransport;
    use eva_core::ErrorKind;
    use eva_storage::{
        DurableBackendOptions, FileSystemDurableBackend, FileSystemProviderProcessTable,
        ProviderProcessTable,
    };
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::thread;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    fn runtime_invokes_skill_adapter_with_controlled_runner() {
        let project = load_project_config(workspace_root()).unwrap();
        let runtime = AdapterRuntime::from_project(&project).unwrap();
        let invocation = AdapterInvocation::new(
            RequestId::parse("req-skill-1").unwrap(),
            CapabilityName::parse("workflow.code_review").unwrap(),
        )
        .with_provider(AdapterId::parse("code-review-skill").unwrap())
        .with_input("{\"scope\":\"current_diff\"}");

        let report = runtime.invoke(invocation).unwrap();

        assert_eq!(report.status, "completed");
        assert!(report.output.contains("code-review"));
        assert!(report.output.contains("builtin_codex_skill"));
        assert_eq!(
            report.trace.request_id.as_ref().map(|id| id.as_str()),
            Some("req-skill-1")
        );
        assert_eq!(
            report.trace.adapter_id.as_ref().map(|id| id.as_str()),
            Some("code-review-skill")
        );
        assert_eq!(
            report
                .trace
                .capability
                .as_ref()
                .map(|capability| capability.as_str()),
            Some("workflow.code_review")
        );
        assert!(report
            .audit
            .iter()
            .any(|entry| entry.starts_with("provider.session:")));
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "provider.health:completed"));
        let processes = runtime.provider_processes().unwrap();
        assert_eq!(processes.len(), 1);
        assert!(!processes[0].active);
        assert_eq!(processes[0].health, "completed");
        assert_eq!(processes[0].adapter_id.as_str(), "code-review-skill");
    }

    #[test]
    fn runtime_invokes_stdio_adapter_with_redacted_env() {
        let env_name = "EVA_TEST_STDIO_SECRET_RUNTIME";
        let secret = "stdio-runtime-secret";
        std::env::set_var(env_name, secret);
        let runtime = runtime_with_handle(stdio_handle(
            true,
            test_command(),
            env_echo_args(env_name),
            vec![env_name.to_owned()],
        ));

        let report = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-stdio-runtime").unwrap(),
                    CapabilityName::parse("repo.analyze").unwrap(),
                )
                .with_provider(AdapterId::parse("stdio-test").unwrap()),
            )
            .unwrap();
        std::env::remove_var(env_name);

        assert_eq!(report.status, "completed");
        assert!(!report.output.contains(secret));
        assert!(!report.output.contains("eva-provider-session:"));
        assert!(report.output.contains("[REDACTED]"));
        assert!(report
            .audit
            .contains(&format!("credential_env:{env_name}:redacted")));
        assert!(report
            .audit
            .contains(&"credential.session_token:redacted".to_owned()));
        assert!(report
            .audit
            .contains(&"policy.action:provider.credential_session".to_owned()));
        assert!(report.audit.contains(&"shell:false".to_owned()));
    }

    #[test]
    fn runtime_rejects_cross_provider_credential_scope_before_start() {
        let handle = stdio_handle(
            true,
            "definitely-not-started",
            Vec::new(),
            vec!["EVA_CROSS_PROVIDER_SECRET".to_owned()],
        );
        let request_id = RequestId::parse("req-cross-provider-scope").unwrap();
        let capability = CapabilityName::parse("repo.analyze").unwrap();
        let scope = ProviderCredentialScope::new_for_session(
            "session-other-req",
            AdapterId::parse("other-provider").unwrap(),
            request_id.clone(),
            capability.clone(),
        );

        let error = dispatch_transport(
            &handle,
            AdapterInvocation::new(request_id, capability).with_credential_scope(scope),
        )
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(error.message().contains("credential session"));
    }

    #[test]
    fn runtime_rejects_disabled_stdio_provider_before_start() {
        let runtime = runtime_with_handle(stdio_handle(
            false,
            "definitely-not-started",
            Vec::new(),
            Vec::new(),
        ));

        let error = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-disabled-stdio").unwrap(),
                    CapabilityName::parse("repo.analyze").unwrap(),
                )
                .with_provider(AdapterId::parse("stdio-test").unwrap()),
            )
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(runtime.provider_processes().unwrap().is_empty());
    }

    #[test]
    fn runtime_releases_provider_slot_when_stdio_start_fails() {
        let runtime = runtime_with_handle(stdio_handle(
            true,
            "definitely-not-started",
            Vec::new(),
            Vec::new(),
        ));

        let error = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-stdio-start-fail").unwrap(),
                    CapabilityName::parse("repo.analyze").unwrap(),
                )
                .with_provider(AdapterId::parse("stdio-test").unwrap()),
            )
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Unavailable);
        let processes = runtime.provider_processes().unwrap();
        assert_eq!(processes.len(), 1);
        assert!(!processes[0].active);
        assert_eq!(processes[0].health, "failed");
        assert!(processes[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("failed to start stdio provider"));
        assert!(processes[0]
            .audit
            .iter()
            .any(|entry| entry == "provider.supervisor.failed"));
    }

    #[test]
    fn runtime_can_mirror_provider_processes_to_durable_table() {
        let root = temp_root("durable-provider-table");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)).unwrap();
        let process_table = FileSystemProviderProcessTable::from_durable_layout(backend.layout());
        let runtime = AdapterRuntime::from_registry_with_provider_process_table(
            registry_with_handle(stdio_handle(
                true,
                "definitely-not-started",
                Vec::new(),
                Vec::new(),
            )),
            process_table,
        );

        let error = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-durable-provider-table").unwrap(),
                    CapabilityName::parse("repo.analyze").unwrap(),
                )
                .with_provider(AdapterId::parse("stdio-test").unwrap()),
            )
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Unavailable);
        let reader = FileSystemProviderProcessTable::from_durable_layout(backend.layout());
        let processes = reader.list().unwrap();
        assert_eq!(processes.len(), 1);
        assert_eq!(
            processes[0].session_id,
            "session-stdio-test-req-durable-provider-table"
        );
        assert!(!processes[0].active);
        assert_eq!(processes[0].health, "failed");

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn runtime_blocks_new_provider_process_while_circuit_is_open() {
        let mut handle = stdio_handle(true, "definitely-not-started", Vec::new(), Vec::new());
        handle.circuit_breaker = Some(AdapterCircuitBreaker {
            failure_threshold: 1,
            recovery_window_ms: 60_000,
        });
        let runtime = runtime_with_handle(handle);

        let first = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-circuit-runtime-a").unwrap(),
                    CapabilityName::parse("repo.analyze").unwrap(),
                )
                .with_provider(AdapterId::parse("stdio-test").unwrap()),
            )
            .unwrap_err();
        let second = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-circuit-runtime-b").unwrap(),
                    CapabilityName::parse("repo.analyze").unwrap(),
                )
                .with_provider(AdapterId::parse("stdio-test").unwrap()),
            )
            .unwrap_err();

        assert_eq!(first.kind(), ErrorKind::Unavailable);
        assert_eq!(second.kind(), ErrorKind::Unavailable);
        assert_eq!(
            second.provider_code().map(|code| code.as_str()),
            Some("provider_circuit_open")
        );
        let processes = runtime.provider_processes().unwrap();
        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].health, "circuit_open");
    }

    #[test]
    fn runtime_invokes_http_adapter_and_redacts_credential_header() {
        let env_name = "EVA_TEST_HTTP_SECRET_RUNTIME";
        let secret = "http-runtime-secret";
        std::env::set_var(env_name, secret);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1/provider", listener.local_addr().unwrap());
        let server_secret = secret.to_owned();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request_bytes = Vec::new();
            let mut buffer = [0_u8; 512];
            let header_end = loop {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    panic!("HTTP test client closed before headers were complete");
                }
                request_bytes.extend_from_slice(&buffer[..read]);
                if let Some(header_end) = http_header_end(&request_bytes) {
                    break header_end;
                }
            };
            let header = String::from_utf8_lossy(&request_bytes[..header_end]);
            let content_length = http_content_length(&header);
            while request_bytes.len().saturating_sub(header_end + 4) < content_length {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                request_bytes.extend_from_slice(&buffer[..read]);
            }
            let request = String::from_utf8_lossy(&request_bytes);
            assert!(request.contains("Authorization: http-runtime-secret"));
            assert!(request.contains(PROVIDER_SESSION_ID_HEADER));
            assert!(request.contains(PROVIDER_SESSION_TOKEN_HEADER));
            assert!(request.contains("{\"message\":\"hello\"}"));
            let session_token =
                http_header_value(&request, PROVIDER_SESSION_TOKEN_HEADER).unwrap_or_default();
            let body = format!("provider echoed {server_secret} {session_token}");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
        });
        let runtime = runtime_with_handle(http_handle(
            endpoint,
            BTreeMap::from([("Authorization".to_owned(), format!("env:{env_name}"))]),
            vec![env_name.to_owned()],
        ));

        let report = runtime
            .invoke(
                AdapterInvocation::new(
                    RequestId::parse("req-http-runtime").unwrap(),
                    CapabilityName::parse("chat.reply").unwrap(),
                )
                .with_provider(AdapterId::parse("http-test").unwrap())
                .with_input("{\"message\":\"hello\"}"),
            )
            .unwrap();
        server.join().unwrap();
        std::env::remove_var(env_name);

        assert_eq!(report.status, "completed");
        assert!(!report.output.contains(secret));
        assert!(!report.output.contains("eva-provider-session:"));
        assert!(report.output.contains("[REDACTED]"));
        assert!(report.audit.contains(&format!(
            "credential_header:Authorization:env:{env_name}:redacted"
        )));
        assert!(report
            .audit
            .contains(&"credential.session_token:redacted".to_owned()));
    }

    fn runtime_with_handle(handle: AdapterHandle) -> AdapterRuntime {
        AdapterRuntime::from_registry(registry_with_handle(handle))
    }

    fn registry_with_handle(handle: AdapterHandle) -> AdapterRegistry {
        let mut registry = AdapterRegistry::new();
        registry.register(handle).unwrap();
        registry
    }

    fn http_header_end(bytes: &[u8]) -> Option<usize> {
        bytes.windows(4).position(|window| window == b"\r\n\r\n")
    }

    fn http_content_length(header: &str) -> usize {
        header
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.eq_ignore_ascii_case("content-length") {
                    value.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0)
    }

    fn http_header_value(request: &str, header_name: &str) -> Option<String> {
        request.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case(header_name) {
                Some(value.trim().to_owned())
            } else {
                None
            }
        })
    }

    fn temp_root(name: &str) -> PathBuf {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "eva-adapter-runtime-{name}-{}-{now}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn stdio_handle(
        enabled: bool,
        command: impl Into<String>,
        args: Vec<String>,
        credential_env: Vec<String>,
    ) -> AdapterHandle {
        AdapterHandle {
            id: AdapterId::parse("stdio-test").unwrap(),
            name: "Stdio Test".to_owned(),
            version: "1.0.0".to_owned(),
            enabled,
            transport: AdapterTransport::Stdio,
            capabilities: vec![CapabilityName::parse("repo.analyze").unwrap()],
            source_path: "test".to_owned(),
            command: Some(command.into()),
            args,
            endpoint: None,
            method: None,
            credential_env,
            timeout_ms: Some(5_000),
            max_concurrency: None,
            output_limit_bytes: Some(4096),
            max_prompt_bytes: Some(4096),
            rate_limit: None,
            circuit_breaker: None,
            headers: BTreeMap::new(),
            mcp_server_transport: None,
            mcp_command: None,
            mcp_args: Vec::new(),
            mcp_tools: Vec::new(),
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

    fn http_handle(
        endpoint: String,
        headers: BTreeMap<String, String>,
        credential_env: Vec<String>,
    ) -> AdapterHandle {
        AdapterHandle {
            id: AdapterId::parse("http-test").unwrap(),
            name: "HTTP Test".to_owned(),
            version: "1.0.0".to_owned(),
            enabled: true,
            transport: AdapterTransport::Http,
            capabilities: vec![CapabilityName::parse("chat.reply").unwrap()],
            source_path: "test".to_owned(),
            command: None,
            args: Vec::new(),
            endpoint: Some(endpoint),
            method: Some("POST".to_owned()),
            credential_env,
            timeout_ms: Some(5_000),
            max_concurrency: None,
            output_limit_bytes: Some(4096),
            max_prompt_bytes: Some(4096),
            rate_limit: None,
            circuit_breaker: None,
            headers,
            mcp_server_transport: None,
            mcp_command: None,
            mcp_args: Vec::new(),
            mcp_tools: Vec::new(),
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

    #[cfg(windows)]
    fn test_command() -> &'static str {
        "powershell"
    }

    #[cfg(not(windows))]
    fn test_command() -> &'static str {
        "sh"
    }

    #[cfg(windows)]
    fn env_echo_args(env_name: &str) -> Vec<String> {
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            format!(
                "[Console]::Out.Write($env:{env_name}); [Console]::Out.Write($env:{PROVIDER_SESSION_TOKEN_ENV}); [Console]::Error.Write($env:{env_name}); [Console]::Error.Write($env:{PROVIDER_SESSION_TOKEN_ENV})"
            ),
        ]
    }

    #[cfg(not(windows))]
    fn env_echo_args(env_name: &str) -> Vec<String> {
        vec![
            "-c".to_owned(),
            format!(
                "printf \"${env_name}\"; printf \"${PROVIDER_SESSION_TOKEN_ENV}\"; printf \"${env_name}\" >&2; printf \"${PROVIDER_SESSION_TOKEN_ENV}\" >&2"
            ),
        ]
    }
}
