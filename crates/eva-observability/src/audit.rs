//! Audit event sink traits and audit field contracts.

use crate::TraceFields;
use eva_core::EvaError;
use std::time::SystemTime;

/// Stable action categories for audit records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AuditAction {
    ConfigLoaded,
    ConfigValidated,
    PolicyEvaluated,
    RuntimeStarted,
    RuntimeRecovered,
    RuntimeControl,
    RuntimeStopped,
    RestoreApply,
    RestoreRollback,
    SchedulerRetry,
    EventAccepted,
    EventDelivered,
    TaskLifecycle,
    LuaHostLog,
    LuaHostAudit,
    CapabilityInvoked,
    AdapterInvoked,
    ProviderSupervised,
    ProviderCredentialSession,
    SkillRunStarted,
    SkillRunCompleted,
    SkillRunFailed,
    McpSessionStarted,
    McpSessionStopped,
    McpStreamAborted,
    McpProxyDenied,
    MemoryWrite,
    MemoryRead,
    MemorySearch,
    MemoryContext,
    MemoryMaintenance,
    HardwareDriverStarted,
    HardwareDriverStopped,
    HardwareHotplugPublished,
    SecurityDenied,
}

/// Stable audit outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AuditOutcome {
    Ok,
    Planned,
    Blocked,
    Failed,
}

/// Audit record that can be written to any future backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    pub action: AuditAction,
    pub outcome: AuditOutcome,
    pub trace: TraceFields,
    pub message: Option<String>,
    pub fields: Vec<(String, String)>,
    pub recorded_at: SystemTime,
}

/// Sink trait implemented by runtime, tests, or storage-backed audit writers.
pub trait AuditSink {
    fn record(&mut self, event: AuditEvent) -> Result<(), EvaError>;
}

/// In-memory sink useful for tests and dry-run flows.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryAuditSink {
    pub events: Vec<AuditEvent>,
}

impl AuditAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConfigLoaded => "config.loaded",
            Self::ConfigValidated => "config.validated",
            Self::PolicyEvaluated => "policy.evaluated",
            Self::RuntimeStarted => "runtime.started",
            Self::RuntimeRecovered => "runtime.recovered",
            Self::RuntimeControl => "runtime.control",
            Self::RuntimeStopped => "runtime.stopped",
            Self::RestoreApply => "restore.apply",
            Self::RestoreRollback => "restore.rollback",
            Self::SchedulerRetry => "scheduler.retry",
            Self::EventAccepted => "event.accepted",
            Self::EventDelivered => "event.delivered",
            Self::TaskLifecycle => "task.lifecycle",
            Self::LuaHostLog => "lua.host.log",
            Self::LuaHostAudit => "lua.host.audit",
            Self::CapabilityInvoked => "capability.invoked",
            Self::AdapterInvoked => "adapter.invoked",
            Self::ProviderSupervised => "provider.supervised",
            Self::ProviderCredentialSession => "provider.credential_session",
            Self::SkillRunStarted => "skill.run.started",
            Self::SkillRunCompleted => "skill.run.completed",
            Self::SkillRunFailed => "skill.run.failed",
            Self::McpSessionStarted => "mcp.session.started",
            Self::McpSessionStopped => "mcp.session.stopped",
            Self::McpStreamAborted => "mcp.stream.aborted",
            Self::McpProxyDenied => "mcp.proxy.denied",
            Self::MemoryWrite => "memory.write",
            Self::MemoryRead => "memory.read",
            Self::MemorySearch => "memory.search",
            Self::MemoryContext => "memory.context",
            Self::MemoryMaintenance => "memory.maintenance",
            Self::HardwareDriverStarted => "hardware.driver.started",
            Self::HardwareDriverStopped => "hardware.driver.stopped",
            Self::HardwareHotplugPublished => "hardware.hotplug.published",
            Self::SecurityDenied => "security.denied",
        }
    }
}

impl AuditOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Planned => "planned",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
        }
    }
}

impl AuditEvent {
    pub fn new(action: AuditAction, outcome: AuditOutcome, trace: TraceFields) -> Self {
        Self {
            action,
            outcome,
            trace,
            message: None,
            fields: Vec::new(),
            recorded_at: SystemTime::now(),
        }
    }

    pub fn with_message(mut self, value: impl Into<String>) -> Self {
        self.message = Some(value.into());
        self
    }

    pub fn with_field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.push((key.into(), value.into()));
        self
    }
}

impl AuditSink for InMemoryAuditSink {
    fn record(&mut self, event: AuditEvent) -> Result<(), EvaError> {
        self.events.push(event);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SpanId;

    #[test]
    fn audit_action_spelling_is_stable() {
        assert_eq!(AuditAction::ConfigValidated.as_str(), "config.validated");
        assert_eq!(AuditAction::RuntimeRecovered.as_str(), "runtime.recovered");
        assert_eq!(AuditAction::RuntimeControl.as_str(), "runtime.control");
        assert_eq!(AuditAction::RestoreApply.as_str(), "restore.apply");
        assert_eq!(AuditAction::RestoreRollback.as_str(), "restore.rollback");
        assert_eq!(AuditAction::SchedulerRetry.as_str(), "scheduler.retry");
        assert_eq!(AuditAction::TaskLifecycle.as_str(), "task.lifecycle");
        assert_eq!(AuditAction::LuaHostLog.as_str(), "lua.host.log");
        assert_eq!(AuditAction::LuaHostAudit.as_str(), "lua.host.audit");
        assert_eq!(
            AuditAction::ProviderSupervised.as_str(),
            "provider.supervised"
        );
        assert_eq!(
            AuditAction::ProviderCredentialSession.as_str(),
            "provider.credential_session"
        );
        assert_eq!(AuditAction::SkillRunStarted.as_str(), "skill.run.started");
        assert_eq!(
            AuditAction::SkillRunCompleted.as_str(),
            "skill.run.completed"
        );
        assert_eq!(AuditAction::SkillRunFailed.as_str(), "skill.run.failed");
        assert_eq!(
            AuditAction::McpSessionStarted.as_str(),
            "mcp.session.started"
        );
        assert_eq!(
            AuditAction::McpSessionStopped.as_str(),
            "mcp.session.stopped"
        );
        assert_eq!(AuditAction::McpStreamAborted.as_str(), "mcp.stream.aborted");
        assert_eq!(AuditAction::McpProxyDenied.as_str(), "mcp.proxy.denied");
        assert_eq!(AuditAction::MemoryWrite.as_str(), "memory.write");
        assert_eq!(AuditAction::MemoryRead.as_str(), "memory.read");
        assert_eq!(AuditAction::MemorySearch.as_str(), "memory.search");
        assert_eq!(AuditAction::MemoryContext.as_str(), "memory.context");
        assert_eq!(
            AuditAction::MemoryMaintenance.as_str(),
            "memory.maintenance"
        );
        assert_eq!(
            AuditAction::HardwareDriverStarted.as_str(),
            "hardware.driver.started"
        );
        assert_eq!(
            AuditAction::HardwareDriverStopped.as_str(),
            "hardware.driver.stopped"
        );
        assert_eq!(
            AuditAction::HardwareHotplugPublished.as_str(),
            "hardware.hotplug.published"
        );
        assert_eq!(AuditOutcome::Blocked.as_str(), "blocked");
    }

    #[test]
    fn in_memory_sink_records_events() {
        let trace = TraceFields::default().with_span_id(SpanId::parse("span-1").unwrap());
        let event = AuditEvent::new(AuditAction::PolicyEvaluated, AuditOutcome::Ok, trace)
            .with_message("policy ok")
            .with_field("layer_count", "3");
        let mut sink = InMemoryAuditSink::default();

        sink.record(event).unwrap();

        assert_eq!(sink.events.len(), 1);
        assert_eq!(sink.events[0].message.as_deref(), Some("policy ok"));
        assert_eq!(sink.events[0].fields[0], ("layer_count".into(), "3".into()));
    }
}
