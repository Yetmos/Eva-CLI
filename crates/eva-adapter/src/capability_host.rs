//! Adapter-backed CapabilityHostApi implementation.

use crate::runtime::{AdapterInvocation, AdapterInvokeReport, AdapterRuntime};
use eva_capability::{CapabilityHostApi, CapabilityRouter};
use eva_core::{
    AdapterId, ErrorKind, EvaError, InvokeOutput, InvokeRequest, InvokeResponse, InvokeTarget,
};
use eva_policy::PermissionSet;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "adapter-backed capability invocation and response normalization";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterBackedCapabilityHost {
    router: CapabilityRouter,
    runtime: AdapterRuntime,
    permissions: PermissionSet,
}

impl AdapterBackedCapabilityHost {
    pub fn new(
        router: CapabilityRouter,
        runtime: AdapterRuntime,
        permissions: PermissionSet,
    ) -> Self {
        Self {
            router,
            runtime,
            permissions,
        }
    }

    pub fn router(&self) -> &CapabilityRouter {
        &self.router
    }

    pub fn runtime(&self) -> &AdapterRuntime {
        &self.runtime
    }

    pub fn permissions(&self) -> &PermissionSet {
        &self.permissions
    }
}

impl CapabilityHostApi for AdapterBackedCapabilityHost {
    fn invoke(&self, request: InvokeRequest) -> Result<InvokeResponse, EvaError> {
        if !matches!(request.target(), InvokeTarget::Capability(_)) {
            return Err(EvaError::invalid_argument(
                "adapter-backed capability host requires capability target",
            ));
        }
        let plan = match self
            .router
            .authorized_provider_plan(&request, None, &self.permissions)
        {
            Ok(plan) => plan,
            Err(error) => return Ok(response_from_error(request.request_id().clone(), error)),
        };
        if plan.is_empty() {
            return Ok(InvokeResponse::failed(
                request.request_id().clone(),
                EvaError::unsupported("capability has no adapter provider plan")
                    .with_context("capability", plan.capability.as_str()),
            ));
        }

        let request_id = request.request_id().clone();
        let mut last_retryable_error = None;
        for candidate in &plan.providers {
            match self.invoke_provider(&request, &candidate.provider) {
                Ok(report) if report.status == "completed" => {
                    return Ok(response_from_report(report))
                }
                Ok(report) => {
                    let error =
                        report_error(report).with_context("provider", candidate.provider.as_str());
                    if is_retryable_provider_failure(&error) {
                        last_retryable_error = Some(error);
                        continue;
                    }
                    return Ok(response_from_error(request_id, error));
                }
                Err(error) if is_retryable_provider_failure(&error) => {
                    last_retryable_error =
                        Some(error.with_context("provider", candidate.provider.as_str()));
                }
                Err(error) => {
                    return Ok(response_from_error(
                        request_id,
                        error.with_context("provider", candidate.provider.as_str()),
                    ));
                }
            }
        }

        Ok(response_from_error(
            request_id,
            last_retryable_error.unwrap_or_else(|| {
                EvaError::unavailable("all adapter providers failed")
                    .with_context("capability", plan.capability.as_str())
            }),
        ))
    }
}

impl AdapterBackedCapabilityHost {
    fn invoke_provider(
        &self,
        request: &InvokeRequest,
        provider: &AdapterId,
    ) -> Result<AdapterInvokeReport, EvaError> {
        let capability = match request.target() {
            InvokeTarget::Capability(capability) => capability.clone(),
            _ => {
                return Err(EvaError::invalid_argument(
                    "adapter-backed capability host requires capability target",
                ))
            }
        };
        let input = request.input().as_text().unwrap_or_default().to_owned();
        let invocation = AdapterInvocation::new(request.request_id().clone(), capability)
            .with_provider(provider.clone())
            .with_input(input);
        self.runtime.invoke(invocation)
    }
}

pub fn response_from_report(report: AdapterInvokeReport) -> InvokeResponse {
    let request_id = report.request_id.clone();
    match report.status.as_str() {
        "completed" => InvokeResponse::completed(request_id, InvokeOutput::text(report.output)),
        "timeout" => InvokeResponse::timeout_with_error(request_id, report_error(report)),
        _ => InvokeResponse::failed(request_id, report_error(report)),
    }
}

pub fn response_from_error(request_id: eva_core::RequestId, error: EvaError) -> InvokeResponse {
    if error.kind() == ErrorKind::Timeout {
        InvokeResponse::timeout_with_error(request_id, error)
    } else {
        InvokeResponse::failed(request_id, error)
    }
}

/// Returns whether adapter-backed capability invocation may try the next provider.
pub fn is_retryable_provider_failure(error: &EvaError) -> bool {
    error.is_retryable()
}

fn report_error(report: AdapterInvokeReport) -> EvaError {
    let kind = match report.status.as_str() {
        "timeout" => ErrorKind::Timeout,
        "output_limit_exceeded" => ErrorKind::Conflict,
        _ => ErrorKind::Unavailable,
    };
    EvaError::new(kind, "adapter provider returned non-completed status")
        .with_provider_code(format!("adapter_status_{}", report.status))
        .with_context("adapter_id", report.adapter_id.as_str())
        .with_context("capability", report.capability.as_str())
        .with_context("transport", report.transport.as_str())
        .with_context("status", report.status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::AdapterHandle;
    use crate::registry::AdapterRegistry;
    use eva_capability::{CapabilityDescriptor, CapabilityProviderSelection, CapabilityRegistry};
    use eva_config::AdapterTransport;
    use eva_core::{CapabilityId, CapabilityName, InvokeInput, InvokeStatus, RequestId};
    use std::collections::BTreeMap;

    fn capability(value: &str) -> CapabilityName {
        CapabilityName::parse(value).unwrap()
    }

    fn adapter(value: &str) -> AdapterId {
        AdapterId::parse(value).unwrap()
    }

    #[test]
    fn response_from_report_maps_completed_report() {
        let response = response_from_report(report("completed", "ok"));

        assert_eq!(response.status(), InvokeStatus::Completed);
        assert_eq!(response.output().unwrap().as_text(), Some("ok"));
    }

    #[test]
    fn response_from_report_maps_timeout_with_safe_context() {
        let response = response_from_report(report("timeout", "late"));

        assert_eq!(response.status(), InvokeStatus::Timeout);
        let error = response.error().unwrap();
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert_eq!(
            error.provider_code().unwrap().as_str(),
            "adapter_status_timeout"
        );
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "adapter_id" && value == "builtin-test"));
    }

    #[test]
    fn adapter_backed_host_invokes_authorized_provider() {
        let host = host_with_builtin_adapter(true);
        let request = InvokeRequest::new(
            RequestId::parse("req-capability-adapter").unwrap(),
            InvokeTarget::Capability(capability("repo.summary")),
            InvokeInput::text("repo"),
        );

        let response = host.invoke(request).unwrap();

        assert_eq!(response.status(), InvokeStatus::Completed);
        assert!(response
            .output()
            .unwrap()
            .as_text()
            .unwrap()
            .contains("controlled-envelope"));
    }

    #[test]
    fn adapter_backed_host_returns_failed_response_for_denied_provider() {
        let host = host_with_permissions(
            PermissionSet::deny_all()
                .allow_capability(capability("repo.summary"))
                .allow_capability(capability("repo.analyze")),
        );
        let request = InvokeRequest::new(
            RequestId::parse("req-capability-denied").unwrap(),
            InvokeTarget::Capability(capability("repo.summary")),
            InvokeInput::text("repo"),
        );

        let response = host.invoke(request).unwrap();

        assert_eq!(response.status(), InvokeStatus::Failed);
        assert_eq!(
            response.error().unwrap().kind(),
            ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn adapter_backed_host_normalizes_disabled_provider_to_failed_response() {
        let host = host_with_builtin_adapter(false);
        let request = InvokeRequest::new(
            RequestId::parse("req-capability-disabled").unwrap(),
            InvokeTarget::Capability(capability("repo.summary")),
            InvokeInput::text("repo"),
        );

        let response = host.invoke(request).unwrap();

        assert_eq!(response.status(), InvokeStatus::Failed);
        assert_eq!(
            response.error().unwrap().kind(),
            ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn adapter_backed_host_falls_back_after_retryable_report_failure() {
        let host = host_with_provider_selection_and_handles(
            PermissionSet::deny_all()
                .allow_capability(capability("repo.summary"))
                .allow_adapter(adapter("stdio-fail"))
                .allow_adapter(adapter("builtin-test")),
            CapabilityProviderSelection::new(
                None,
                Some(adapter("stdio-fail")),
                vec![adapter("builtin-test")],
                Vec::new(),
            ),
            vec![
                stdio_handle("stdio-fail", Some(test_command()), fail_args()),
                builtin_handle(true),
            ],
        );
        let request = InvokeRequest::new(
            RequestId::parse("req-capability-fallback").unwrap(),
            InvokeTarget::Capability(capability("repo.summary")),
            InvokeInput::text("repo"),
        );

        let response = host.invoke(request).unwrap();

        assert_eq!(response.status(), InvokeStatus::Completed);
        assert!(response
            .output()
            .unwrap()
            .as_text()
            .unwrap()
            .contains("builtin-test"));
    }

    #[test]
    fn adapter_backed_host_stops_after_non_retryable_provider_error() {
        let host = host_with_provider_selection_and_handles(
            PermissionSet::deny_all()
                .allow_capability(capability("repo.summary"))
                .allow_adapter(adapter("stdio-invalid"))
                .allow_adapter(adapter("builtin-test")),
            CapabilityProviderSelection::new(
                None,
                Some(adapter("stdio-invalid")),
                vec![adapter("builtin-test")],
                Vec::new(),
            ),
            vec![
                stdio_handle("stdio-invalid", None, Vec::new()),
                builtin_handle(true),
            ],
        );
        let request = InvokeRequest::new(
            RequestId::parse("req-capability-nonretryable").unwrap(),
            InvokeTarget::Capability(capability("repo.summary")),
            InvokeInput::text("repo"),
        );

        let response = host.invoke(request).unwrap();

        assert_eq!(response.status(), InvokeStatus::Failed);
        let error = response.error().unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert!(!error.is_retryable());
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "provider" && value == "stdio-invalid"));
    }

    #[test]
    fn adapter_backed_host_preserves_last_retryable_error_when_all_providers_fail() {
        let host = host_with_provider_selection_and_handles(
            PermissionSet::deny_all()
                .allow_capability(capability("repo.summary"))
                .allow_adapter(adapter("stdio-fail-a"))
                .allow_adapter(adapter("stdio-fail-b")),
            CapabilityProviderSelection::new(
                None,
                Some(adapter("stdio-fail-a")),
                vec![adapter("stdio-fail-b")],
                Vec::new(),
            ),
            vec![
                stdio_handle("stdio-fail-a", Some(test_command()), fail_args()),
                stdio_handle("stdio-fail-b", Some(test_command()), fail_args()),
            ],
        );
        let request = InvokeRequest::new(
            RequestId::parse("req-capability-all-fail").unwrap(),
            InvokeTarget::Capability(capability("repo.summary")),
            InvokeInput::text("repo"),
        );

        let response = host.invoke(request).unwrap();

        assert_eq!(response.status(), InvokeStatus::Failed);
        let error = response.error().unwrap();
        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert!(error.is_retryable());
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "provider" && value == "stdio-fail-b"));
    }

    fn host_with_builtin_adapter(enabled: bool) -> AdapterBackedCapabilityHost {
        host_with_permissions_and_handle(
            PermissionSet::deny_all()
                .allow_capability(capability("repo.summary"))
                .allow_capability(capability("repo.analyze"))
                .allow_adapter(adapter("builtin-test")),
            builtin_handle(enabled),
        )
    }

    fn host_with_permissions(permissions: PermissionSet) -> AdapterBackedCapabilityHost {
        host_with_permissions_and_handle(permissions, builtin_handle(true))
    }

    fn host_with_permissions_and_handle(
        permissions: PermissionSet,
        handle: AdapterHandle,
    ) -> AdapterBackedCapabilityHost {
        host_with_provider_selection_and_handles(
            permissions,
            CapabilityProviderSelection::new(
                None,
                Some(adapter("builtin-test")),
                Vec::new(),
                vec![capability("repo.analyze")],
            ),
            vec![handle],
        )
    }

    fn host_with_provider_selection_and_handles(
        permissions: PermissionSet,
        provider_selection: CapabilityProviderSelection,
        handles: Vec<AdapterHandle>,
    ) -> AdapterBackedCapabilityHost {
        let mut capability_registry = CapabilityRegistry::new();
        capability_registry
            .register(CapabilityDescriptor {
                id: CapabilityId::parse("repo-summary-test").unwrap(),
                name: capability("repo.summary"),
                enabled: true,
                provider: "builtin-test".to_owned(),
                provider_selection,
            })
            .unwrap();
        let mut adapter_registry = AdapterRegistry::new();
        for handle in handles {
            adapter_registry.register(handle).unwrap();
        }
        let runtime = AdapterRuntime::from_registry(adapter_registry);
        AdapterBackedCapabilityHost::new(
            CapabilityRouter::new(capability_registry),
            runtime,
            permissions,
        )
    }

    fn builtin_handle(enabled: bool) -> AdapterHandle {
        AdapterHandle {
            id: adapter("builtin-test"),
            name: "Builtin Test".to_owned(),
            version: "1.0.0".to_owned(),
            enabled,
            transport: AdapterTransport::Builtin,
            capabilities: vec![capability("repo.summary")],
            source_path: "test".to_owned(),
            command: None,
            args: Vec::new(),
            endpoint: None,
            method: None,
            credential_env: Vec::new(),
            timeout_ms: Some(5_000),
            output_limit_bytes: Some(4096),
            max_prompt_bytes: Some(4096),
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
            bindings: Vec::new(),
        }
    }

    fn stdio_handle(id: &str, command: Option<&str>, args: Vec<String>) -> AdapterHandle {
        let mut handle = builtin_handle(true);
        handle.id = adapter(id);
        handle.name = format!("Stdio Test {id}");
        handle.transport = AdapterTransport::Stdio;
        handle.command = command.map(str::to_owned);
        handle.args = args;
        handle
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
    fn fail_args() -> Vec<String> {
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            "exit 7".to_owned(),
        ]
    }

    #[cfg(not(windows))]
    fn fail_args() -> Vec<String> {
        vec!["-c".to_owned(), "exit 7".to_owned()]
    }

    fn report(status: &str, output: &str) -> AdapterInvokeReport {
        AdapterInvokeReport {
            request_id: RequestId::parse("req-report").unwrap(),
            adapter_id: adapter("builtin-test"),
            transport: AdapterTransport::Builtin,
            capability: capability("repo.summary"),
            status: status.to_owned(),
            output: output.to_owned(),
            audit: Vec::new(),
            trace: Default::default(),
        }
    }
}
