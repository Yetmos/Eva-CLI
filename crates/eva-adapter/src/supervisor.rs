//! Provider supervisor slots and process table integration.

use crate::manifest::AdapterHandle;
use crate::runtime::AdapterInvocation;
use eva_config::AdapterTransport;
use eva_core::{AdapterId, CapabilityName, EvaError, RequestId};
use eva_storage::{InMemoryProviderProcessTable, ProviderProcessSnapshot, ProviderProcessTable};
use std::collections::BTreeMap;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "provider execution slots and process table mutation";
pub const PROVIDER_SESSION_ID_ENV: &str = "EVA_PROVIDER_SESSION_ID";
pub const PROVIDER_SESSION_TOKEN_ENV: &str = "EVA_PROVIDER_SESSION_TOKEN";
pub const PROVIDER_SESSION_ID_HEADER: &str = "X-Eva-Provider-Session";
pub const PROVIDER_SESSION_TOKEN_HEADER: &str = "X-Eva-Provider-Session-Token";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderExecutionRequest {
    pub request_id: RequestId,
    pub adapter_id: AdapterId,
    pub capability: CapabilityName,
    pub transport: AdapterTransport,
    pub manifest_digest: String,
    pub start_command: String,
    pub restart_policy: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderExecutionSlot {
    pub session_id: String,
    pub provider_process_id: String,
    pub request_id: RequestId,
    pub adapter_id: AdapterId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCredentialScope {
    pub session_id: String,
    pub adapter_id: AdapterId,
    pub request_id: RequestId,
    pub capability: CapabilityName,
    pub token_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderExecutionOutcome {
    pub health: String,
    pub last_error: Option<String>,
}

pub trait ProviderSupervisor {
    fn acquire(
        &mut self,
        request: ProviderExecutionRequest,
    ) -> Result<ProviderExecutionSlot, EvaError>;
    fn complete(
        &mut self,
        slot: &ProviderExecutionSlot,
        outcome: ProviderExecutionOutcome,
    ) -> Result<ProviderProcessSnapshot, EvaError>;
    fn processes(&self) -> Result<Vec<ProviderProcessSnapshot>, EvaError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InMemoryProviderSupervisor {
    table: InMemoryProviderProcessTable,
}

impl ProviderExecutionRequest {
    pub fn from_handle(handle: &AdapterHandle, invocation: &AdapterInvocation) -> Self {
        Self {
            request_id: invocation.request_id.clone(),
            adapter_id: handle.id.clone(),
            capability: invocation.capability.clone(),
            transport: handle.transport,
            manifest_digest: manifest_digest(handle),
            start_command: start_command(handle),
            restart_policy: "none".to_owned(),
        }
    }
}

impl ProviderCredentialScope {
    pub fn new_for_session(
        session_id: impl Into<String>,
        adapter_id: AdapterId,
        request_id: RequestId,
        capability: CapabilityName,
    ) -> Self {
        let session_id = session_id.into();
        let token_digest =
            credential_token_digest(&session_id, &adapter_id, &request_id, &capability);
        Self {
            session_id,
            adapter_id,
            request_id,
            capability,
            token_digest,
        }
    }

    pub fn from_slot(slot: &ProviderExecutionSlot, capability: CapabilityName) -> Self {
        Self::new_for_session(
            slot.session_id.clone(),
            slot.adapter_id.clone(),
            slot.request_id.clone(),
            capability,
        )
    }

    pub fn ensure_matches(
        &self,
        adapter_id: &AdapterId,
        request_id: &RequestId,
        capability: &CapabilityName,
    ) -> Result<(), EvaError> {
        if &self.adapter_id != adapter_id {
            return Err(EvaError::permission_denied(
                "provider credential session cannot be reused across providers",
            )
            .with_context("session_id", &self.session_id)
            .with_context("session_provider", self.adapter_id.as_str())
            .with_context("requested_provider", adapter_id.as_str()));
        }
        if &self.request_id != request_id || &self.capability != capability {
            return Err(EvaError::permission_denied(
                "provider credential session cannot be reused across requests",
            )
            .with_context("session_id", &self.session_id)
            .with_context("session_request", self.request_id.as_str())
            .with_context("requested_request", request_id.as_str()));
        }
        Ok(())
    }

    pub fn audit_entries(&self) -> Vec<String> {
        vec![
            "credential.scope:provider_session".to_owned(),
            format!("credential.session:{}", self.session_id),
            format!("credential.session_digest:{}", self.token_digest),
            "credential.session_token:redacted".to_owned(),
        ]
    }

    pub(crate) fn apply_env(&self, env: &mut BTreeMap<String, String>) {
        env.insert(PROVIDER_SESSION_ID_ENV.to_owned(), self.session_id.clone());
        env.insert(PROVIDER_SESSION_TOKEN_ENV.to_owned(), self.session_token());
    }

    pub(crate) fn apply_headers(&self, headers: &mut BTreeMap<String, String>) {
        headers.insert(
            PROVIDER_SESSION_ID_HEADER.to_owned(),
            self.session_id.clone(),
        );
        headers.insert(
            PROVIDER_SESSION_TOKEN_HEADER.to_owned(),
            self.session_token(),
        );
    }

    pub(crate) fn redaction_values(&self) -> Vec<String> {
        vec![self.session_token()]
    }

    fn session_token(&self) -> String {
        format!(
            "eva-provider-session:{}:{}",
            self.session_id, self.token_digest
        )
    }
}

impl ProviderExecutionOutcome {
    pub fn completed(status: &str) -> Self {
        Self {
            health: if status == "completed" {
                "completed".to_owned()
            } else {
                "failed".to_owned()
            },
            last_error: if status == "completed" {
                None
            } else {
                Some(format!("adapter returned status {status}"))
            },
        }
    }

    pub fn failed(error: &EvaError) -> Self {
        Self {
            health: "failed".to_owned(),
            last_error: Some(format!("{}: {}", error.kind().as_str(), error.message())),
        }
    }
}

pub(crate) fn validate_credential_scope_for_provider<'a>(
    scope: Option<&'a ProviderCredentialScope>,
    adapter_id: &AdapterId,
    request_id: &RequestId,
    capability: &CapabilityName,
    required: bool,
) -> Result<Option<&'a ProviderCredentialScope>, EvaError> {
    match scope {
        Some(scope) => {
            scope.ensure_matches(adapter_id, request_id, capability)?;
            Ok(Some(scope))
        }
        None if required => Err(EvaError::permission_denied(
            "provider credential session scope is required",
        )
        .with_context("adapter_id", adapter_id.as_str())
        .with_context("request_id", request_id.as_str())),
        None => Ok(None),
    }
}

pub(crate) fn redact_provider_session_tokens(value: &str) -> String {
    let mut redacted = value.to_owned();
    while let Some(start) = redacted.find("eva-provider-session:") {
        let end = redacted[start..]
            .char_indices()
            .find_map(|(offset, ch)| {
                if offset > 0 && (ch.is_whitespace() || matches!(ch, '"' | '\'' | '\\' | '<' | '>'))
                {
                    Some(start + offset)
                } else {
                    None
                }
            })
            .unwrap_or(redacted.len());
        redacted.replace_range(start..end, "[REDACTED]");
    }
    redacted
}

impl Default for InMemoryProviderSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryProviderSupervisor {
    pub fn new() -> Self {
        Self {
            table: InMemoryProviderProcessTable::new(),
        }
    }

    pub fn active_for_adapter(
        &self,
        adapter_id: &AdapterId,
    ) -> Result<Vec<ProviderProcessSnapshot>, EvaError> {
        self.table.active_for_adapter(adapter_id)
    }
}

impl ProviderSupervisor for InMemoryProviderSupervisor {
    fn acquire(
        &mut self,
        request: ProviderExecutionRequest,
    ) -> Result<ProviderExecutionSlot, EvaError> {
        let session_id = session_id(&request.request_id, &request.adapter_id);
        let provider_process_id = provider_process_id(&request.request_id, &request.adapter_id);
        let snapshot = ProviderProcessSnapshot::running(
            session_id.clone(),
            provider_process_id.clone(),
            request.request_id.clone(),
            request.adapter_id.clone(),
            request.capability,
            request.transport.as_str(),
            request.manifest_digest,
            request.start_command,
            request.restart_policy,
        );
        self.table.upsert(snapshot)?;
        Ok(ProviderExecutionSlot {
            session_id,
            provider_process_id,
            request_id: request.request_id,
            adapter_id: request.adapter_id,
        })
    }

    fn complete(
        &mut self,
        slot: &ProviderExecutionSlot,
        outcome: ProviderExecutionOutcome,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        let mut snapshot = self.table.read(&slot.session_id)?;
        snapshot.release(outcome.health, outcome.last_error)?;
        self.table.upsert(snapshot.clone())?;
        Ok(snapshot)
    }

    fn processes(&self) -> Result<Vec<ProviderProcessSnapshot>, EvaError> {
        self.table.list()
    }
}

fn session_id(request_id: &RequestId, adapter_id: &AdapterId) -> String {
    format!(
        "session-{}-{}",
        safe_segment(adapter_id.as_str()),
        safe_segment(request_id.as_str())
    )
}

fn provider_process_id(request_id: &RequestId, adapter_id: &AdapterId) -> String {
    format!(
        "provider-{}-{}",
        safe_segment(adapter_id.as_str()),
        safe_segment(request_id.as_str())
    )
}

fn start_command(handle: &AdapterHandle) -> String {
    match handle.transport {
        AdapterTransport::Stdio => command_with_args(handle.command.as_deref(), &handle.args),
        AdapterTransport::Http => handle
            .endpoint
            .as_ref()
            .map(|endpoint| {
                format!(
                    "{} {}",
                    handle.method.as_deref().unwrap_or("POST"),
                    endpoint
                )
            })
            .unwrap_or_else(|| "http:<missing-endpoint>".to_owned()),
        AdapterTransport::Mcp => command_with_args(handle.mcp_command.as_deref(), &handle.mcp_args),
        AdapterTransport::Skill => {
            if let Some(command) = handle.skill_runner_command.as_deref() {
                command_with_args(Some(command), &handle.skill_runner_args)
            } else if let Some(command) = handle.command.as_deref() {
                command_with_args(Some(command), &handle.args)
            } else {
                format!(
                    "skill:{}",
                    handle.skill_name().unwrap_or("<missing-skill-id>")
                )
            }
        }
        AdapterTransport::Builtin => "builtin".to_owned(),
        AdapterTransport::Hardware => "hardware-driver".to_owned(),
        AdapterTransport::LuaCapability => "lua-capability".to_owned(),
        AdapterTransport::Eventbus => "eventbus".to_owned(),
    }
}

fn command_with_args(command: Option<&str>, args: &[String]) -> String {
    let command = command.unwrap_or("<missing-command>");
    if args.is_empty() {
        command.to_owned()
    } else {
        format!("{command} {}", args.join(" "))
    }
}

fn manifest_digest(handle: &AdapterHandle) -> String {
    let material = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}",
        handle.id.as_str(),
        handle.version,
        handle.transport.as_str(),
        handle.source_path,
        handle.command.as_deref().unwrap_or(""),
        handle.endpoint.as_deref().unwrap_or(""),
        handle.mcp_command.as_deref().unwrap_or(""),
        handle.skill_name().unwrap_or(""),
        handle
            .capabilities
            .iter()
            .map(|capability| capability.as_str())
            .collect::<Vec<_>>()
            .join(",")
    );
    format!("fnv64:{:016x}", fnv1a64(material.as_bytes()))
}

fn credential_token_digest(
    session_id: &str,
    adapter_id: &AdapterId,
    request_id: &RequestId,
    capability: &CapabilityName,
) -> String {
    let material = format!(
        "{session_id}|{}|{}|{}",
        adapter_id.as_str(),
        request_id.as_str(),
        capability.as_str()
    );
    format!("fnv64:{:016x}", fnv1a64(material.as_bytes()))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn safe_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::AdapterHandle;
    use std::collections::BTreeMap;

    fn handle() -> AdapterHandle {
        AdapterHandle {
            id: AdapterId::parse("stdio-test").unwrap(),
            name: "Stdio Test".to_owned(),
            version: "1.0.0".to_owned(),
            enabled: true,
            transport: AdapterTransport::Stdio,
            capabilities: vec![CapabilityName::parse("repo.analyze").unwrap()],
            source_path: "test".to_owned(),
            command: Some("stdio-runner".to_owned()),
            args: vec!["--once".to_owned()],
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
            hardware_driver_id: None,
            hardware_driver_kind: None,
            bindings: Vec::new(),
        }
    }

    #[test]
    fn supervisor_records_acquire_and_release() {
        let handle = handle();
        let invocation = AdapterInvocation::new(
            RequestId::parse("req-supervisor-1").unwrap(),
            CapabilityName::parse("repo.analyze").unwrap(),
        );
        let mut supervisor = InMemoryProviderSupervisor::new();

        let slot = supervisor
            .acquire(ProviderExecutionRequest::from_handle(&handle, &invocation))
            .unwrap();
        assert_eq!(supervisor.active_for_adapter(&handle.id).unwrap().len(), 1);

        let snapshot = supervisor
            .complete(&slot, ProviderExecutionOutcome::completed("completed"))
            .unwrap();

        assert!(!snapshot.active);
        assert_eq!(snapshot.health, "completed");
        assert_eq!(supervisor.active_for_adapter(&handle.id).unwrap().len(), 0);
        assert!(snapshot
            .audit
            .iter()
            .any(|entry| entry == "provider.slot:released"));
    }

    #[test]
    fn skill_start_command_uses_matching_fallback_args() {
        let mut handle = handle();
        handle.transport = AdapterTransport::Skill;
        handle.command = Some("skill-fallback".to_owned());
        handle.args = vec!["--fallback".to_owned()];
        handle.skill_runner_command = None;
        handle.skill_runner_args = vec!["--runner".to_owned()];

        let request = ProviderExecutionRequest::from_handle(
            &handle,
            &AdapterInvocation::new(
                RequestId::parse("req-supervisor-2").unwrap(),
                CapabilityName::parse("repo.analyze").unwrap(),
            ),
        );

        assert_eq!(request.start_command, "skill-fallback --fallback");
    }

    #[test]
    fn credential_scope_rejects_cross_provider_reuse() {
        let scope = ProviderCredentialScope::new_for_session(
            "session-stdio-req",
            AdapterId::parse("stdio-test").unwrap(),
            RequestId::parse("req-supervisor-credentials").unwrap(),
            CapabilityName::parse("repo.analyze").unwrap(),
        );

        let error = scope
            .ensure_matches(
                &AdapterId::parse("other-provider").unwrap(),
                &RequestId::parse("req-supervisor-credentials").unwrap(),
                &CapabilityName::parse("repo.analyze").unwrap(),
            )
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::PermissionDenied);
        assert!(!scope
            .audit_entries()
            .iter()
            .any(|entry| entry.contains("eva-provider-session:")));
    }

    #[test]
    fn credential_scope_injects_token_without_exposing_it_in_audit() {
        let scope = ProviderCredentialScope::new_for_session(
            "session-stdio-req",
            AdapterId::parse("stdio-test").unwrap(),
            RequestId::parse("req-supervisor-credentials").unwrap(),
            CapabilityName::parse("repo.analyze").unwrap(),
        );
        let mut env = BTreeMap::new();
        let mut headers = BTreeMap::new();

        scope.apply_env(&mut env);
        scope.apply_headers(&mut headers);

        assert_eq!(
            env.get(PROVIDER_SESSION_ID_ENV).map(String::as_str),
            Some("session-stdio-req")
        );
        assert!(env
            .get(PROVIDER_SESSION_TOKEN_ENV)
            .unwrap()
            .starts_with("eva-provider-session:"));
        assert!(headers.contains_key(PROVIDER_SESSION_TOKEN_HEADER));
        assert!(scope
            .audit_entries()
            .contains(&"credential.session_token:redacted".to_owned()));
    }

    #[test]
    fn provider_session_token_redaction_catches_prefixed_values() {
        let redacted = redact_provider_session_tokens(
            "before eva-provider-session:session-1:fnv64:abc123 after",
        );

        assert_eq!(redacted, "before [REDACTED] after");
    }
}
