//! 无副作用的发现健康探测。
//! Side-effect-free discovery health probing.

/// 本模块的架构职责：根据发现候选项生成健康状态，不连接或执行候选能力。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "health probing for discovered candidates";

/// 候选项在发现边界内的可见状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryHealthStatus {
    /// 已被来源发现，且来源没有给出拒绝原因。
    Seen,
    /// 已被发现但不满足来源的信任或配置约束。
    Rejected,
}

/// 单个候选项的只读健康记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryHealth {
    /// 与规范化候选项一致的稳定标识。
    pub candidate_id: String,
    /// 候选项的发现状态。
    pub status: DiscoveryHealthStatus,
    /// 面向诊断的拒绝原因或边界说明。
    pub message: String,
}

impl DiscoveryHealthStatus {
    /// 返回用于报告和序列化展示的稳定状态字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Seen => "seen",
            Self::Rejected => "rejected",
        }
    }
}
