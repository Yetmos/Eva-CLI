//! Data-only routing records used by the scheduler.

use eva_core::{AgentId, TopicPattern};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "data-only Topic routing rules";

/// Delivery mode selected for a route match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeliveryMode {
    Fanout,
    Compete,
}

/// One normalized route rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingRule {
    pub pattern: TopicPattern,
    pub delivery: DeliveryMode,
    pub agents: Vec<AgentId>,
}

impl DeliveryMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fanout => "fanout",
            Self::Compete => "compete",
        }
    }
}

impl RoutingRule {
    pub fn new(pattern: TopicPattern, delivery: DeliveryMode, agents: Vec<AgentId>) -> Self {
        Self {
            pattern,
            delivery,
            agents,
        }
    }
}
