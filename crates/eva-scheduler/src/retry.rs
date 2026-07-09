//! Retry dispatch helpers for redriven events.

use crate::{DeliveryPlan, MailboxRegistry, SubscriptionTable};
use eva_core::{EvaError, Event};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "dispatch redriven events through scheduler mailboxes";

/// Evidence returned after a retry event has been handed to scheduler mailboxes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryDispatchReport {
    pub deliveries: Vec<DeliveryPlan>,
}

pub fn dispatch_retry_event(
    table: &SubscriptionTable,
    registry: &mut MailboxRegistry,
    event: &Event,
) -> Result<RetryDispatchReport, EvaError> {
    let deliveries = table.deliver(registry, event)?;
    Ok(RetryDispatchReport { deliveries })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeliveryMode, RoutingRule};
    use eva_core::{AgentId, EventId, EventPayload, Topic, TopicPattern};

    fn event(topic: &str) -> Event {
        Event::new(
            EventId::parse("evt-retry-1").unwrap(),
            Topic::parse(topic).unwrap(),
            EventPayload::empty(),
        )
    }

    #[test]
    fn retry_dispatch_delivers_to_mailbox() {
        let agent = AgentId::parse("root-agent").unwrap();
        let table = SubscriptionTable::new(vec![RoutingRule::new(
            TopicPattern::parse("/input/user").unwrap(),
            DeliveryMode::Fanout,
            vec![agent.clone()],
        )]);
        let mut registry = MailboxRegistry::new();
        registry.register(agent.clone(), 1).unwrap();

        let report = dispatch_retry_event(&table, &mut registry, &event("/input/user")).unwrap();

        assert_eq!(report.deliveries.len(), 1);
        assert_eq!(report.deliveries[0].agent_id, agent);
        assert_eq!(registry.mailbox(&agent).unwrap().len(), 1);
    }

    #[test]
    fn retry_dispatch_reports_route_failure_without_delivery() {
        let table = SubscriptionTable::new(Vec::new());
        let mut registry = MailboxRegistry::new();

        let error = dispatch_retry_event(&table, &mut registry, &event("/input/user")).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::NotFound);
        assert!(registry.is_empty());
    }
}
