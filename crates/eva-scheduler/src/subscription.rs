//! 中文：Agent 订阅表、事件路由展开和邮箱投递计划。
//! Agent subscription tables and event delivery planning.

use crate::matcher::matching_rules;
use crate::registry::MailboxRegistry;
use crate::routing::{DeliveryMode, RoutingRule};
use eva_core::{AgentId, EvaError, Event, EventTarget};

/// 中文：本模块负责把目标事件或主题规则解析成可执行的 Agent 投递计划。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Agent subscription tables";

/// 中文：路由规则展开后的一次具体 Agent 投递。
/// One planned delivery after routing expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryPlan {
    /// 中文：接收该事件的 Agent 标识。
    pub agent_id: AgentId,
    /// 中文：产生本次投递的路由模式，供审计说明投递来源。
    pub delivery: DeliveryMode,
}

/// 中文：把事件解析为 Agent 邮箱投递的调度订阅表。
/// Scheduler table that resolves events into Agent mailbox deliveries.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SubscriptionTable {
    /// 中文：按配置优先顺序保存的规范化路由规则。
    rules: Vec<RoutingRule>,
}

impl SubscriptionTable {
    /// 中文：使用给定的有序规则创建订阅表。
    pub fn new(rules: Vec<RoutingRule>) -> Self {
        Self { rules }
    }

    /// 中文：返回订阅规则的只读切片，顺序与构造时一致。
    pub fn rules(&self) -> &[RoutingRule] {
        &self.rules
    }

    /// 中文：把事件解析成一个或多个投递计划，但不修改邮箱。
    ///
    /// 显式 Agent 目标优先于主题规则，以保证点对点命令不会被普通订阅扩散；普通事件
    /// 按规则顺序展开，`Fanout` 选择全部 Agent，`Compete` 确定性选择首个 Agent。
    /// 最终没有任何计划时返回未找到错误，避免事件被静默丢弃。
    pub fn route(&self, event: &Event) -> Result<Vec<DeliveryPlan>, EvaError> {
        if let EventTarget::Agent(agent_id) = event.target() {
            return Ok(vec![DeliveryPlan {
                agent_id: agent_id.clone(),
                delivery: DeliveryMode::Fanout,
            }]);
        }

        let mut plans = Vec::new();
        for rule in matching_rules(&self.rules, event.topic()) {
            match rule.delivery {
                DeliveryMode::Fanout => {
                    plans.extend(rule.agents.iter().cloned().map(|agent_id| DeliveryPlan {
                        agent_id,
                        delivery: DeliveryMode::Fanout,
                    }))
                }
                DeliveryMode::Compete => {
                    if let Some(agent_id) = rule.agents.first() {
                        plans.push(DeliveryPlan {
                            agent_id: agent_id.clone(),
                            delivery: DeliveryMode::Compete,
                        });
                    }
                }
            }
        }

        if plans.is_empty() {
            return Err(
                EvaError::not_found("no scheduler route matched event topic")
                    .with_context("topic", event.topic().as_str()),
            );
        }
        Ok(plans)
    }

    /// 中文：先生成完整路由计划，再按顺序把事件副本写入各 Agent 邮箱。
    ///
    /// 任一邮箱不存在或满载时立即返回错误；已经完成的前序投递不会回滚，调用方需根据
    /// 返回错误和事件幂等性决定是否重试。
    pub fn deliver(
        &self,
        registry: &mut MailboxRegistry,
        event: &Event,
    ) -> Result<Vec<DeliveryPlan>, EvaError> {
        let plans = self.route(event)?;
        for plan in &plans {
            registry.deliver(&plan.agent_id, event.clone())?;
        }
        Ok(plans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::RoutingRule;
    use eva_core::{EventId, EventPayload, Topic, TopicPattern};

    /// 中文：构造订阅路由测试使用的事件。
    fn event(topic: &str) -> Event {
        Event::new(
            EventId::parse("evt-1").unwrap(),
            Topic::parse(topic).unwrap(),
            EventPayload::empty(),
        )
    }

    #[test]
    /// 中文：验证广播规则会为每个配置 Agent 生成投递计划。
    fn fanout_route_returns_all_agents() {
        let table = SubscriptionTable::new(vec![RoutingRule::new(
            TopicPattern::parse("/input/user").unwrap(),
            DeliveryMode::Fanout,
            vec![
                AgentId::parse("root-agent").unwrap(),
                AgentId::parse("agent-a").unwrap(),
            ],
        )]);

        let plans = table.route(&event("/input/user")).unwrap();

        assert_eq!(plans.len(), 2);
    }

    #[test]
    /// 中文：验证显式 Agent 目标无需主题规则即可直接路由。
    fn direct_target_overrides_topic_rules() {
        let table = SubscriptionTable::new(Vec::new());
        let event = event("/missing")
            .with_target(EventTarget::Agent(AgentId::parse("root-agent").unwrap()));

        let plans = table.route(&event).unwrap();

        assert_eq!(plans[0].agent_id.as_str(), "root-agent");
    }

    #[test]
    /// 中文：验证执行投递计划会把事件写入对应已注册邮箱。
    fn deliver_pushes_into_registered_mailbox() {
        let table = SubscriptionTable::new(vec![RoutingRule::new(
            TopicPattern::parse("/input/user").unwrap(),
            DeliveryMode::Fanout,
            vec![AgentId::parse("root-agent").unwrap()],
        )]);
        let mut registry = MailboxRegistry::new();
        let agent_id = AgentId::parse("root-agent").unwrap();
        registry.register(agent_id.clone(), 2).unwrap();

        table.deliver(&mut registry, &event("/input/user")).unwrap();

        assert_eq!(registry.mailbox(&agent_id).unwrap().len(), 1);
    }
}
