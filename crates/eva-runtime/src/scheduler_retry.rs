//! 中文：V1.12.4 基于持久化死信退避状态执行一次调度器重试 Tick。
//! V1.12.4 scheduler retry tick over durable dead-letter backoff.

use eva_config::{ProjectConfig, RouteDelivery};
use eva_core::{AgentId, EvaError, Event, EventId};
use eva_eventbus::{DurableEventBus, EventBus};
use eva_scheduler::{
    dispatch_retry_event, DeliveryMode, DeliveryPlan, MailboxRegistry, RoutingRule,
    SubscriptionTable,
};
use eva_storage::EventLogStatus;

/// 中文：本模块筛选到期死信，经标准订阅表重新投递并持久化确认或失败状态。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "dispatch due durable dead-letter retries through scheduler";

/// 中文：单次重试 Tick 为每个启用 Agent 创建的默认临时邮箱容量。
const DEFAULT_MAILBOX_CAPACITY: usize = 256;
/// 中文：写入重放事件确认和失败记录的稳定系统消费者标识。
const SCHEDULER_RETRY_CONSUMER: &str = "scheduler-retry";

/// 中文：一次调度器重试扫描的时间阈值和临时邮箱容量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerRetryTickOptions {
    /// 中文：重驱记录必须不晚于此毫秒阈值才视为到期。
    pub redrive_ready_at_ms: u64,
    /// 中文：每个启用 Agent 的临时投递邮箱容量。
    pub mailbox_capacity: usize,
}

/// 中文：一次重试 Tick 的扫描计数、分组结果和审计轨迹。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerRetryTickReport {
    /// 中文：本次读取的死信记录总数。
    pub scanned_dead_letters: usize,
    /// 中文：达到时间阈值的死信数，包括随后因状态被跳过的记录。
    pub due_dead_letters: usize,
    /// 中文：成功投递并确认的重放事件。
    pub dispatched_events: Vec<SchedulerRetryDispatchedEvent>,
    /// 中文：已生成重放事件但调度投递失败的记录。
    pub failed_events: Vec<SchedulerRetryFailedEvent>,
    /// 中文：因未到期、已确认或已有重放状态而跳过的记录。
    pub skipped_events: Vec<SchedulerRetrySkippedEvent>,
    /// 中文：按执行顺序保存的稳定审计消息。
    pub audit: Vec<String>,
}

/// 中文：一个成功进入 Agent 邮箱并在事件日志确认的重放事件摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerRetryDispatchedEvent {
    /// 中文：原始死信事件标识。
    pub event_id: String,
    /// 中文：本次生成的唯一重放事件标识。
    pub replay_event_id: String,
    /// 中文：重放事件日志序号。
    pub sequence: u64,
    /// 中文：重放事件主题。
    pub topic: String,
    /// 中文：写入确认状态的系统消费者。
    pub ack_consumer: String,
    /// 中文：标准订阅表产生的实际 Agent 投递计划。
    pub deliveries: Vec<DeliveryPlan>,
}

/// 中文：重放事件已经持久化、但调度投递失败的摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerRetryFailedEvent {
    /// 中文：原始死信事件标识。
    pub event_id: String,
    /// 中文：失败的重放事件标识。
    pub replay_event_id: String,
    /// 中文：重放事件日志序号。
    pub sequence: u64,
    /// 中文：结构化失败类型名称。
    pub reason_kind: String,
    /// 中文：面向操作员的失败原因。
    pub reason: String,
}

/// 中文：未进入重放和投递阶段的死信摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerRetrySkippedEvent {
    /// 中文：被跳过的原始死信事件标识。
    pub event_id: String,
    /// 中文：稳定的跳过原因代码。
    pub reason: String,
}

impl Default for SchedulerRetryTickOptions {
    /// 中文：默认扫描立即到期记录，并使用标准邮箱容量。
    fn default() -> Self {
        Self {
            redrive_ready_at_ms: 0,
            mailbox_capacity: DEFAULT_MAILBOX_CAPACITY,
        }
    }
}

/// 中文：执行一次持久化死信重试扫描、重放、调度和日志状态更新。
///
/// 处理顺序严格遵守持久化事实：先排除未到期、日志缺失、已确认和已有重放记录的死信；
/// 对剩余记录先生成并持久化唯一重放事件，再走普通订阅表投递。投递成功后确认重放记录，
/// 失败则写入失败状态。已有重放无论成功、进行中或失败都不会在同一死信上重复生成事件，
/// 从而使进程重启后的 Tick 保持幂等。
pub fn run_scheduler_retry_tick(
    project: &ProjectConfig,
    bus: &mut DurableEventBus,
    options: SchedulerRetryTickOptions,
) -> Result<SchedulerRetryTickReport, EvaError> {
    let dead_letters = bus.dead_letters().to_vec();
    let mut report = SchedulerRetryTickReport {
        scanned_dead_letters: dead_letters.len(),
        due_dead_letters: 0,
        dispatched_events: Vec::new(),
        failed_events: Vec::new(),
        skipped_events: Vec::new(),
        audit: vec![format!("scheduler.retry:scanned:{}", dead_letters.len())],
    };
    let table = subscription_table(project);
    let mut mailboxes = register_mailboxes(project, options.mailbox_capacity)?;

    for dead_letter in dead_letters {
        let event_id = dead_letter.event_id().clone();
        if dead_letter.redrive.next_attempt_after_ms > options.redrive_ready_at_ms {
            skip_retry(&mut report, &event_id, "redrive_not_due");
            continue;
        }
        report.due_dead_letters += 1;

        if bus.event_log_status(&event_id).is_none() {
            skip_retry(&mut report, &event_id, "event_log_missing");
            continue;
        }
        if bus.event_log_status(&event_id) == Some(EventLogStatus::Acked) {
            skip_retry(&mut report, &event_id, "already_acked");
            continue;
        }
        if let Some(replay) = bus.latest_replay_record(&event_id) {
            let reason = match replay.status {
                EventLogStatus::Acked => "already_dispatched",
                EventLogStatus::Appended => "replay_in_flight",
                EventLogStatus::Failed => "replay_failed",
            };
            skip_retry(&mut report, &event_id, reason);
            continue;
        }

        let receipt = bus.redrive_dead_letter(&event_id)?;
        let replay_event = replay_event(bus, &receipt.event_id)?;
        match dispatch_retry_event(&table, &mut mailboxes, &replay_event) {
            Ok(dispatch) => {
                let ack_consumer = scheduler_retry_consumer()?;
                bus.ack(&receipt.event_id, ack_consumer.clone())?;
                report
                    .audit
                    .push(format!("scheduler.retry:acked:{}", receipt.event_id));
                report
                    .dispatched_events
                    .push(SchedulerRetryDispatchedEvent {
                        event_id: event_id.as_str().to_owned(),
                        replay_event_id: receipt.event_id.as_str().to_owned(),
                        sequence: receipt.sequence,
                        topic: receipt.topic.as_str().to_owned(),
                        ack_consumer: ack_consumer.as_str().to_owned(),
                        deliveries: dispatch.deliveries,
                    });
            }
            Err(error) => {
                bus.fail(
                    &receipt.event_id,
                    scheduler_retry_consumer()?,
                    error.clone(),
                )?;
                report
                    .audit
                    .push(format!("scheduler.retry:failed:{}", receipt.event_id));
                report.failed_events.push(SchedulerRetryFailedEvent {
                    event_id: event_id.as_str().to_owned(),
                    replay_event_id: receipt.event_id.as_str().to_owned(),
                    sequence: receipt.sequence,
                    reason_kind: error.kind().as_str().to_owned(),
                    reason: error.message().to_owned(),
                });
            }
        }
    }

    report
        .audit
        .push(format!("scheduler.retry:due:{}", report.due_dead_letters));
    report.audit.push(format!(
        "scheduler.retry:dispatched:{}",
        report.dispatched_events.len()
    ));
    report.audit.push(format!(
        "scheduler.retry:failed:{}",
        report.failed_events.len()
    ));
    report.audit.push(format!(
        "scheduler.retry:skipped:{}",
        report.skipped_events.len()
    ));
    Ok(report)
}

/// 中文：从事件日志读取刚持久化的重放事件；缺失表示持久化顺序被破坏。
fn replay_event(bus: &DurableEventBus, event_id: &EventId) -> Result<Event, EvaError> {
    bus.event_log_record(event_id)
        .map(|record| record.event.clone())
        .ok_or_else(|| {
            EvaError::internal("redriven event was not persisted before scheduler dispatch")
                .with_context("event_id", event_id.as_str())
        })
}

/// 中文：向报告追加一条不修改总线状态的跳过记录。
fn skip_retry(report: &mut SchedulerRetryTickReport, event_id: &EventId, reason: &'static str) {
    report.skipped_events.push(SchedulerRetrySkippedEvent {
        event_id: event_id.as_str().to_owned(),
        reason: reason.to_owned(),
    });
}

/// 中文：解析写入重放确认记录的稳定系统消费者标识。
fn scheduler_retry_consumer() -> Result<AgentId, EvaError> {
    AgentId::parse(SCHEDULER_RETRY_CONSUMER)
}

/// 中文：把项目配置路由转换为调度器使用的等价订阅表。
fn subscription_table(project: &ProjectConfig) -> SubscriptionTable {
    let rules = project
        .routes
        .routes
        .iter()
        .map(|route| {
            RoutingRule::new(
                route.pattern.clone(),
                match route.delivery {
                    RouteDelivery::Fanout => DeliveryMode::Fanout,
                    RouteDelivery::Compete => DeliveryMode::Compete,
                },
                route.agents.clone(),
            )
        })
        .collect();
    SubscriptionTable::new(rules)
}

/// 中文：为所有启用 Agent 创建相同容量的临时邮箱，供本次 Tick 执行背压检查。
fn register_mailboxes(
    project: &ProjectConfig,
    mailbox_capacity: usize,
) -> Result<MailboxRegistry, EvaError> {
    let mut registry = MailboxRegistry::new();
    for agent in project.agents.iter().filter(|agent| agent.enabled) {
        registry.register(agent.id.clone(), mailbox_capacity)?;
    }
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use eva_core::{EventPayload, RequestId, Topic};
    use eva_eventbus::RedrivePolicy;
    use eva_storage::{DurableBackendOptions, FileSystemDurableBackend};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 中文：返回调度器重试测试使用的工作区根目录。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    /// 中文：创建按进程和纳秒隔离的持久化测试目录。
    fn test_root(name: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "eva-runtime-scheduler-retry-{name}-{}-{now}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    /// 中文：加载仓库样例项目配置。
    fn project() -> ProjectConfig {
        load_project_config(workspace_root()).unwrap()
    }

    /// 中文：构造携带稳定请求标识的重试测试事件。
    fn event(id: &str, topic: &str) -> Event {
        Event::new(
            EventId::parse(id).unwrap(),
            Topic::parse(topic).unwrap(),
            EventPayload::empty(),
        )
        .with_request_id(RequestId::parse("req-retry").unwrap())
    }

    #[test]
    /// 中文：验证未达到退避阈值的死信不会生成重放事件。
    fn scheduler_retry_tick_skips_backoff_that_is_not_due() {
        let root = test_root("not-due");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)).unwrap();
        let mut bus = DurableEventBus::open(backend.layout()).unwrap();
        let original = event("evt-retry-not-due", "/input/user");
        bus.publish(original.clone()).unwrap();
        bus.dead_letter(original.clone(), EvaError::timeout("handler timeout"))
            .unwrap();
        bus.set_dead_letter_redrive_policy(
            original.event_id(),
            RedrivePolicy {
                retry_delay_ms: 5_000,
                next_attempt_after_ms: 5_000,
            },
        )
        .unwrap();

        let report = run_scheduler_retry_tick(
            &project(),
            &mut bus,
            SchedulerRetryTickOptions {
                redrive_ready_at_ms: 1_000,
                ..SchedulerRetryTickOptions::default()
            },
        )
        .unwrap();

        assert_eq!(report.scanned_dead_letters, 1);
        assert_eq!(report.due_dead_letters, 0);
        assert!(report.dispatched_events.is_empty());
        assert_eq!(report.skipped_events[0].reason, "redrive_not_due");
        assert_eq!(bus.dead_letters()[0].replay_count, 0);
        assert_eq!(bus.log().records().len(), 1);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    /// 中文：验证到期死信跨重复 Tick 和后端重开也只投递一次。
    fn scheduler_retry_tick_dispatches_due_event_once_across_reopen() {
        let root = test_root("dispatch-once");
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)).unwrap();
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();
            let original = event("evt-retry-due", "/input/user");
            bus.publish(original.clone()).unwrap();
            bus.dead_letter(original, EvaError::timeout("handler timeout"))
                .unwrap();

            let report = run_scheduler_retry_tick(
                &project(),
                &mut bus,
                SchedulerRetryTickOptions::default(),
            )
            .unwrap();

            assert_eq!(report.due_dead_letters, 1);
            assert_eq!(report.dispatched_events.len(), 1);
            assert_eq!(report.dispatched_events[0].event_id, "evt-retry-due");
            assert_eq!(
                report.dispatched_events[0].replay_event_id,
                "evt-retry-due:replay-1"
            );
            assert_eq!(report.dispatched_events[0].ack_consumer, "scheduler-retry");
            assert_eq!(
                bus.event_log_status(&EventId::parse("evt-retry-due:replay-1").unwrap()),
                Some(EventLogStatus::Acked)
            );

            let second = run_scheduler_retry_tick(
                &project(),
                &mut bus,
                SchedulerRetryTickOptions::default(),
            )
            .unwrap();
            assert!(second.dispatched_events.is_empty());
            assert_eq!(second.skipped_events[0].reason, "already_dispatched");
            assert_eq!(bus.log().records().len(), 2);
        }

        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)).unwrap();
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();

            let report = run_scheduler_retry_tick(
                &project(),
                &mut bus,
                SchedulerRetryTickOptions::default(),
            )
            .unwrap();

            assert!(report.dispatched_events.is_empty());
            assert_eq!(report.skipped_events[0].reason, "already_dispatched");
            assert_eq!(bus.log().records().len(), 2);
        }

        fs::remove_dir_all(root).ok();
    }

    #[test]
    /// 中文：验证无匹配路由时，重放事件在持久化日志中标记为失败。
    fn scheduler_retry_tick_marks_dispatch_failure_in_durable_log() {
        let root = test_root("dispatch-fail");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)).unwrap();
        let mut bus = DurableEventBus::open(backend.layout()).unwrap();
        let original = event("evt-retry-no-route", "/no/route");
        bus.publish(original.clone()).unwrap();
        bus.dead_letter(original, EvaError::timeout("handler timeout"))
            .unwrap();

        let report =
            run_scheduler_retry_tick(&project(), &mut bus, SchedulerRetryTickOptions::default())
                .unwrap();

        assert!(report.dispatched_events.is_empty());
        assert_eq!(report.failed_events.len(), 1);
        assert_eq!(report.failed_events[0].reason_kind, "not_found");
        let replay = bus
            .event_log_record(&EventId::parse("evt-retry-no-route:replay-1").unwrap())
            .unwrap();
        assert_eq!(replay.status, EventLogStatus::Failed);
        assert_eq!(
            replay.consumer.as_ref().map(AgentId::as_str),
            Some(SCHEDULER_RETRY_CONSUMER)
        );

        fs::remove_dir_all(root).ok();
    }
}
