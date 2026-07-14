//! 中文：失败事件重新驱动时的退避决策和调度投递辅助逻辑。
//! Retry dispatch helpers for redriven events.

use crate::{DeliveryPlan, MailboxRegistry, SubscriptionTable};
use eva_core::{EvaError, Event};

/// 中文：本模块负责把重试事件重新送入调度邮箱，并给出确定性的退避决策。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "dispatch redriven events through scheduler mailboxes";

/// 中文：重试事件成功交给调度邮箱后返回的投递证据。
/// Evidence returned after a retry event has been handed to scheduler mailboxes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryDispatchReport {
    /// 中文：本次重试实际生成的全部 Agent 投递计划。
    pub deliveries: Vec<DeliveryPlan>,
}

/// 中文：固定间隔重试策略；尝试次数达到上限后不再入队。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryBackoffPolicy {
    /// 中文：允许执行的最大累计尝试次数。
    pub max_attempts: u32,
    /// 中文：下一次尝试相对当前时间的固定延迟毫秒数。
    pub backoff_ms: u64,
}

/// 中文：对一次失败是否应进入重试队列的完整决策结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryBackoffDecision {
    /// 中文：是否应创建下一次重试任务。
    pub enqueue: bool,
    /// 中文：若继续重试，对应的下一次尝试序号；拒绝时保持当前计数。
    pub next_attempt: u32,
    /// 中文：应等待的毫秒数；不重试时为 `None`。
    pub due_after_ms: Option<u64>,
    /// 中文：供诊断和审计使用的稳定决策原因。
    pub reason: String,
}

/// 中文：按普通订阅规则重新投递失败事件，并返回实际投递计划。
///
/// 此函数不绕过路由和邮箱容量检查，因此重试与首次投递具有相同的隔离和背压语义。
pub fn dispatch_retry_event(
    table: &SubscriptionTable,
    registry: &mut MailboxRegistry,
    event: &Event,
) -> Result<RetryDispatchReport, EvaError> {
    let deliveries = table.deliver(registry, event)?;
    Ok(RetryDispatchReport { deliveries })
}

/// 中文：根据错误可重试性、已尝试次数和策略计算下一步。
///
/// 不可重试错误优先终止；累计次数达到上限同样终止。计数递增使用饱和加法，
/// 防止极端输入在整数上限处回绕成较小的尝试次数。
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

    /// 中文：构造指定主题的重试测试事件。
    fn event(topic: &str) -> Event {
        Event::new(
            EventId::parse("evt-retry-1").unwrap(),
            Topic::parse(topic).unwrap(),
            EventPayload::empty(),
        )
    }

    #[test]
    /// 中文：验证重试事件沿订阅表进入已注册邮箱。
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
    /// 中文：验证无匹配规则时返回错误且不产生邮箱副作用。
    fn retry_dispatch_reports_route_failure_without_delivery() {
        let table = SubscriptionTable::new(Vec::new());
        let mut registry = MailboxRegistry::new();

        let error = dispatch_retry_event(&table, &mut registry, &event("/input/user")).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::NotFound);
        assert!(registry.is_empty());
    }

    #[test]
    /// 中文：验证可重试、次数耗尽和不可重试三类退避决策。
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
