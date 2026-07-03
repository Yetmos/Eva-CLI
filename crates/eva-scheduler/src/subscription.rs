//! Agent subscription tables and event delivery planning.

use crate::matcher::matching_rules;
use crate::registry::MailboxRegistry;
use crate::routing::{DeliveryMode, RoutingRule};
use eva_core::{AgentId, EvaError, Event, EventTarget};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Agent subscription tables";

/// One planned delivery after routing expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryPlan {
    pub agent_id: AgentId,
    pub delivery: DeliveryMode,
}

/// Scheduler table that resolves events into Agent mailbox deliveries.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SubscriptionTable {
    rules: Vec<RoutingRule>,
}

impl SubscriptionTable {
    pub fn new(rules: Vec<RoutingRule>) -> Self {
        Self { rules }
    }

    pub fn rules(&self) -> &[RoutingRule] {
        &self.rules
    }

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

    fn event(topic: &str) -> Event {
        Event::new(
            EventId::parse("evt-1").unwrap(),
            Topic::parse(topic).unwrap(),
            EventPayload::empty(),
        )
    }

    #[test]
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
    fn direct_target_overrides_topic_rules() {
        let table = SubscriptionTable::new(Vec::new());
        let event = event("/missing")
            .with_target(EventTarget::Agent(AgentId::parse("root-agent").unwrap()));

        let plans = table.route(&event).unwrap();

        assert_eq!(plans[0].agent_id.as_str(), "root-agent");
    }

    #[test]
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
