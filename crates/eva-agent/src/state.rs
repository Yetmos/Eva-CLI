//! Agent-local state ownership.

use eva_core::AgentId;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Agent-local state ownership";

/// Small read-only Agent state snapshot exposed for reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStateSnapshot {
    pub agent_id: AgentId,
    pub queued_events: usize,
    pub lifecycle: String,
}

impl AgentStateSnapshot {
    pub fn new(agent_id: AgentId, queued_events: usize, lifecycle: impl Into<String>) -> Self {
        Self {
            agent_id,
            queued_events,
            lifecycle: lifecycle.into(),
        }
    }
}
