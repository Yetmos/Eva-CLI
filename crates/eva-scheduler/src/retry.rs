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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryBackoffPolicy {
    pub max_attempts: u32,
    pub backoff_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryBackoffDecision {
    pub enqueue: bool,
    pub next_attempt: u32,
    pub due_after_ms: Option<u64>,
    pub reason: String,
}

pub fn dispatch_retry_event(
    table: &SubscriptionTable,
    registry: &mut MailboxRegistry,
    event: &Event,
) -> Result<RetryDispatchReport, EvaError> {
    let deliveries = table.deliver(registry, event)?;
    Ok(RetryDispatchReport { deliveries })
}

pub fn decide_retry_backoff(
    retryable: bool,
    attempts_made: u32,
    policy: RetryBackoffPolicy,
) -> RetryBackoffDecision {
    if !retryable {
        return RetryBackoffDecision {
            enqueue: false,
            next_attempt: attempts_made,
            due_after_ms: None,
            reason: "failure is not retryable".to_owned(),
        };
    }
    if attempts_made >= policy.max_attempts {
        return RetryBackoffDecision {
            enqueue: false,
            next_attempt: attempts_made,
            due_after_ms: None,
            reason: "retry attempts exhausted".to_owned(),
        };
    }
    RetryBackoffDecision {
        enqueue: true,
        next_attempt: attempts_made.saturating_add(1),
        due_after_ms: Some(policy.backoff_ms),
        reason: "retryable failure scheduled with backoff".to_owned(),
    }
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

    #[test]
    fn retry_backoff_admits_retryable_failures_until_attempts_exhausted() {
        let policy = RetryBackoffPolicy {
            max_attempts: 2,
            backoff_ms: 1000,
        };

        let first = decide_retry_backoff(true, 0, policy);
        let exhausted = decide_retry_backoff(true, 2, policy);
        let non_retryable = decide_retry_backoff(false, 0, policy);

        assert!(first.enqueue);
        assert_eq!(first.next_attempt, 1);
        assert_eq!(first.due_after_ms, Some(1000));
        assert!(!exhausted.enqueue);
        assert_eq!(exhausted.reason, "retry attempts exhausted");
        assert!(!non_retryable.enqueue);
        assert_eq!(non_retryable.reason, "failure is not retryable");
    }
}
