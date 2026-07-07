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
    RuntimeStopped,
    EventAccepted,
    EventDelivered,
    LuaHostLog,
    LuaHostAudit,
    CapabilityInvoked,
    AdapterInvoked,
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
            Self::RuntimeStopped => "runtime.stopped",
            Self::EventAccepted => "event.accepted",
            Self::EventDelivered => "event.delivered",
            Self::LuaHostLog => "lua.host.log",
            Self::LuaHostAudit => "lua.host.audit",
            Self::CapabilityInvoked => "capability.invoked",
            Self::AdapterInvoked => "adapter.invoked",
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
        assert_eq!(AuditAction::LuaHostLog.as_str(), "lua.host.log");
        assert_eq!(AuditAction::LuaHostAudit.as_str(), "lua.host.audit");
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
