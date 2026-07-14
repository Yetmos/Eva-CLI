//! 中文：调度器使用的纯数据路由契约。
//! Data-only routing records used by the scheduler.

use eva_core::{AgentId, TopicPattern};

/// 中文：本模块只描述主题路由规则，不承担匹配或投递副作用。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "data-only Topic routing rules";

/// 中文：某条规则匹配后采用的投递模式。
/// Delivery mode selected for a route match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeliveryMode {
    /// 中文：把事件复制给规则中的每个 Agent。
    Fanout,
    /// 中文：由规则中的一个 Agent 竞争处理；当前确定性选择首个 Agent。
    Compete,
}

/// 中文：一条已规范化的主题路由规则。
/// One normalized route rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingRule {
    /// 中文：待匹配的主题模式。
    pub pattern: TopicPattern,
    /// 中文：匹配成功后的投递方式。
    pub delivery: DeliveryMode,
    /// 中文：按配置顺序排列的候选 Agent。
    pub agents: Vec<AgentId>,
}

impl DeliveryMode {
    /// 中文：返回用于配置、日志和协议输出的稳定模式名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fanout => "fanout",
            Self::Compete => "compete",
        }
    }
}

impl RoutingRule {
    /// 中文：从已校验的主题模式、投递方式和 Agent 列表构造规则。
    pub fn new(pattern: TopicPattern, delivery: DeliveryMode, agents: Vec<AgentId>) -> Self {
        Self {
            pattern,
            delivery,
            agents,
        }
    }
}
