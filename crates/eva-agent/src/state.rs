//! 中文：Agent 本地状态的只读快照契约。
//! Agent-local state ownership.

use eva_core::AgentId;

/// 中文：本模块负责向诊断层暴露最小且不可变的 Agent 状态摘要。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Agent-local state ownership";

/// 中文：用于报告和诊断的轻量只读 Agent 状态快照。
/// Small read-only Agent state snapshot exposed for reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStateSnapshot {
    /// 中文：快照所属的 Agent 标识。
    pub agent_id: AgentId,
    /// 中文：采样时仍在队列中的事件数量。
    pub queued_events: usize,
    /// 中文：采样时的稳定生命周期名称。
    pub lifecycle: String,
}

impl AgentStateSnapshot {
    /// 中文：从 Agent 标识、队列长度和生命周期名称创建状态快照。
    pub fn new(agent_id: AgentId, queued_events: usize, lifecycle: impl Into<String>) -> Self {
        Self {
            agent_id,
            queued_events,
            lifecycle: lifecycle.into(),
        }
    }
}
