//! 中文：V1.12.4 基于持久化死信退避状态执行一次调度器重试 Tick。
//! V1.12.4 scheduler retry tick over durable dead-letter backoff.

use eva_config::{ProjectConfig, RouteDelivery};
use eva_core::{AgentId, EvaError, Event, EventId};
use eva_eventbus::{
    DeadLetterRecord, DurableEventBus, EventBus, EventReceipt, ReplayHandlerBinding,
};
use eva_scheduler::{DeliveryMode, DeliveryPlan, RoutingRule, SubscriptionTable};
use eva_storage::EventLogStatus;

use crate::{OwnedReplayDeliveryStatus, OwnedReplayHandler};

/// 中文：本模块筛选到期死信，经标准订阅表重新投递并持久化确认或失败状态。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "dispatch due durable dead-letter retries through scheduler";

/// Default upper bound for the number of owned deliveries in one replay plan.
const DEFAULT_MAILBOX_CAPACITY: usize = 256;
/// Aggregate consumer used only when a replay has more than one handler owner.
const SCHEDULER_RETRY_CONSUMER: &str = "scheduler-retry";

/// Timing threshold and bounded delivery count for one scheduler retry tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerRetryTickOptions {
    /// 中文：重驱记录必须不晚于此毫秒阈值才视为到期。
    pub redrive_ready_at_ms: u64,
    /// Legacy field name retained as the maximum owned deliveries accepted per replay.
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

/// A replay whose complete owned-handler plan succeeded before durable ACK.
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
    /// Scans immediately due records and applies the standard delivery-plan bound.
    fn default() -> Self {
        Self {
            redrive_ready_at_ms: 0,
            mailbox_capacity: DEFAULT_MAILBOX_CAPACITY,
        }
    }
}

/// Executes a retry tick without an owned handler.
///
/// This compatibility entry point deliberately fails bound work closed. Daemon execution must use
/// [`run_scheduler_retry_tick_with_handler`] and inject its existing worker owner.
pub fn run_scheduler_retry_tick(
    project: &ProjectConfig,
    bus: &mut DurableEventBus,
    options: SchedulerRetryTickOptions,
) -> Result<SchedulerRetryTickReport, EvaError> {
    run_scheduler_retry_tick_with_handler(project, bus, &MissingOwnedReplayHandler, options)
}

/// Redrives due dead letters through explicit daemon-owned handlers and ACKs only after success.
///
/// An existing `Appended` or `Failed` replay is reused after restart. Only an `Acked` replay is
/// terminal. Handler bindings are persisted with the dead letter and must exactly match the current
/// ordered route plan; the scheduler never guesses a handler kind from an event topic.
pub fn run_scheduler_retry_tick_with_handler(
    project: &ProjectConfig,
    bus: &mut DurableEventBus,
    handler: &dyn OwnedReplayHandler,
    options: SchedulerRetryTickOptions,
) -> Result<SchedulerRetryTickReport, EvaError> {
    if options.mailbox_capacity == 0 {
        return Err(EvaError::invalid_argument(
            "scheduler retry capacity must be greater than zero",
        ));
    }
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
        let receipt = match bus.latest_replay_record(&event_id).cloned() {
            Some(replay) if replay.status == EventLogStatus::Acked => {
                skip_retry(&mut report, &event_id, "already_dispatched");
                continue;
            }
            Some(replay) => EventReceipt::from_record(&replay),
            None => bus.redrive_dead_letter_at(&event_id, options.redrive_ready_at_ms)?,
        };
        let replay_event = replay_event(bus, &receipt.event_id)?;
        let execution = reconcile_replay_plan(
            project,
            handler,
            &dead_letter,
            &table,
            &replay_event,
            options,
            &mut report.audit,
        );

        match execution {
            Ok(ReplayPlanStatus::Succeeded(deliveries)) => {
                let ack_consumer = replay_ack_consumer(&dead_letter.replay_handlers)?;
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
                        deliveries,
                    });
            }
            Ok(ReplayPlanStatus::Pending) => {
                skip_retry(&mut report, &event_id, "handler_pending");
                report.audit.push(format!(
                    "scheduler.retry:handler_pending:{}",
                    receipt.event_id
                ));
            }
            Ok(ReplayPlanStatus::Failed(error)) | Err(error) => {
                let failure_consumer = replay_failure_consumer(
                    &dead_letter.replay_handlers,
                    error
                        .context()
                        .entries()
                        .iter()
                        .find(|(key, _)| key == "agent_id")
                        .map(|(_, value)| value.as_str()),
                )?;
                bus.fail(&receipt.event_id, failure_consumer, error.clone())?;
                if dead_letter.redrive.retry_delay_ms > 0 {
                    bus.set_dead_letter_redrive_policy(
                        &event_id,
                        eva_eventbus::RedrivePolicy {
                            retry_delay_ms: dead_letter.redrive.retry_delay_ms,
                            next_attempt_after_ms: options
                                .redrive_ready_at_ms
                                .saturating_add(dead_letter.redrive.retry_delay_ms),
                        },
                    )?;
                }
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

struct MissingOwnedReplayHandler;

impl OwnedReplayHandler for MissingOwnedReplayHandler {
    fn reconcile_replay_delivery(
        &self,
        binding: &ReplayHandlerBinding,
        event: &Event,
        delivery_index: usize,
        _retry_backoff_ms: u64,
        _observed_at_ms: u64,
    ) -> Result<OwnedReplayDeliveryStatus, EvaError> {
        Err(EvaError::not_found("owned replay handler is not available")
            .with_context("handler_kind", binding.handler_kind())
            .with_context("agent_id", binding.agent_id().as_str())
            .with_context("event_id", event.event_id().as_str())
            .with_context("delivery_index", delivery_index.to_string()))
    }
}

enum ReplayPlanStatus {
    Pending,
    Succeeded(Vec<DeliveryPlan>),
    Failed(EvaError),
}

fn reconcile_replay_plan(
    project: &ProjectConfig,
    handler: &dyn OwnedReplayHandler,
    dead_letter: &DeadLetterRecord,
    table: &SubscriptionTable,
    replay_event: &Event,
    options: SchedulerRetryTickOptions,
    audit: &mut Vec<String>,
) -> Result<ReplayPlanStatus, EvaError> {
    let deliveries = table.route(replay_event)?;
    validate_replay_bindings(
        project,
        &dead_letter.replay_handlers,
        &deliveries,
        options.mailbox_capacity,
    )?;
    let mut pending = false;
    let mut failure = None;
    for (delivery_index, binding) in dead_letter.replay_handlers.iter().enumerate() {
        match handler
            .reconcile_replay_delivery(
                binding,
                replay_event,
                delivery_index,
                dead_letter.redrive.retry_delay_ms,
                options.redrive_ready_at_ms,
            )
            .map_err(|error| {
                error
                    .with_context("handler_kind", binding.handler_kind())
                    .with_context("agent_id", binding.agent_id().as_str())
                    .with_context("delivery_index", delivery_index.to_string())
            })? {
            OwnedReplayDeliveryStatus::Pending { task_id, status } => {
                pending = true;
                audit.push(format!(
                    "scheduler.retry:delivery_pending:{delivery_index}:{task_id}:{status}"
                ));
            }
            OwnedReplayDeliveryStatus::Succeeded {
                task_id,
                result_digest,
                result_size_bytes,
            } => audit.push(format!(
                "scheduler.retry:delivery_succeeded:{delivery_index}:{task_id}:{result_digest}:{result_size_bytes}"
            )),
            OwnedReplayDeliveryStatus::Failed {
                task_id,
                error,
                retry_scheduled,
            } => {
                audit.push(format!(
                    "scheduler.retry:delivery_failed:{delivery_index}:{task_id}:{}:{retry_scheduled}",
                    error.kind().as_str()
                ));
                if failure.is_none() {
                    failure = Some(
                        error
                            .with_context("agent_id", binding.agent_id().as_str())
                            .with_context("delivery_index", delivery_index.to_string()),
                    );
                }
            }
        }
    }
    if let Some(error) = failure {
        Ok(ReplayPlanStatus::Failed(error))
    } else if pending {
        Ok(ReplayPlanStatus::Pending)
    } else {
        Ok(ReplayPlanStatus::Succeeded(deliveries))
    }
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

fn replay_ack_consumer(bindings: &[ReplayHandlerBinding]) -> Result<AgentId, EvaError> {
    match bindings {
        [binding] => Ok(binding.agent_id().clone()),
        _ => scheduler_retry_consumer(),
    }
}

fn replay_failure_consumer(
    bindings: &[ReplayHandlerBinding],
    failed_agent_id: Option<&str>,
) -> Result<AgentId, EvaError> {
    if let Some(agent_id) = failed_agent_id {
        if let Some(binding) = bindings
            .iter()
            .find(|binding| binding.agent_id().as_str() == agent_id)
        {
            return Ok(binding.agent_id().clone());
        }
    }
    replay_ack_consumer(bindings)
}

fn validate_replay_bindings(
    project: &ProjectConfig,
    bindings: &[ReplayHandlerBinding],
    deliveries: &[DeliveryPlan],
    max_deliveries: usize,
) -> Result<(), EvaError> {
    if bindings.is_empty() {
        return Err(EvaError::not_found(
            "dead-letter replay handler binding is missing",
        ));
    }
    if bindings.len() != deliveries.len() {
        return Err(EvaError::conflict(
            "dead-letter replay handler plan does not match scheduler route",
        )
        .with_context("binding_count", bindings.len().to_string())
        .with_context("delivery_count", deliveries.len().to_string()));
    }
    if deliveries.len() > max_deliveries {
        return Err(EvaError::conflict(
            "dead-letter replay handler plan exceeds the configured delivery bound",
        )
        .with_context("delivery_count", deliveries.len().to_string())
        .with_context("max_deliveries", max_deliveries.to_string()));
    }
    for (index, (binding, delivery)) in bindings.iter().zip(deliveries).enumerate() {
        if binding.agent_id() != &delivery.agent_id {
            return Err(EvaError::conflict(
                "dead-letter replay handler owner does not match scheduler delivery",
            )
            .with_context("delivery_index", index.to_string())
            .with_context("binding_agent_id", binding.agent_id().as_str())
            .with_context("delivery_agent_id", delivery.agent_id.as_str()));
        }
        if !project
            .agents
            .iter()
            .any(|agent| agent.enabled && agent.id == delivery.agent_id)
        {
            return Err(
                EvaError::not_found("dead-letter replay handler Agent is not enabled")
                    .with_context("agent_id", delivery.agent_id.as_str()),
            );
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use eva_core::{EventPayload, RequestId, Topic};
    use eva_eventbus::{RedrivePolicy, ReplayHandlerBinding};
    use eva_storage::{DurableBackendOptions, FileSystemDurableBackend, FileSystemTaskStateStore};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};
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

    fn root_echo_binding() -> ReplayHandlerBinding {
        ReplayHandlerBinding::new("runtime.echo", AgentId::parse("root-agent").unwrap()).unwrap()
    }

    struct CountingReplayHandler {
        invocations: Arc<AtomicUsize>,
        fail_first: bool,
    }

    impl CountingReplayHandler {
        fn new(invocations: Arc<AtomicUsize>, fail_first: bool) -> Self {
            Self {
                invocations,
                fail_first,
            }
        }
    }

    impl OwnedReplayHandler for CountingReplayHandler {
        fn reconcile_replay_delivery(
            &self,
            _binding: &ReplayHandlerBinding,
            _event: &Event,
            delivery_index: usize,
            _retry_backoff_ms: u64,
            _observed_at_ms: u64,
        ) -> Result<OwnedReplayDeliveryStatus, EvaError> {
            let attempt = self.invocations.fetch_add(1, Ordering::SeqCst);
            let task_id = format!("test-replay-delivery-{delivery_index}");
            if self.fail_first && attempt == 0 {
                Ok(OwnedReplayDeliveryStatus::Failed {
                    task_id,
                    error: EvaError::timeout("owned replay handler failed"),
                    retry_scheduled: true,
                })
            } else {
                Ok(OwnedReplayDeliveryStatus::Succeeded {
                    task_id,
                    result_digest: format!("sha256:{}", "0".repeat(64)),
                    result_size_bytes: 0,
                })
            }
        }
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
        let invocations = Arc::new(AtomicUsize::new(0));
        let handler = CountingReplayHandler::new(Arc::clone(&invocations), false);
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)).unwrap();
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();
            let original = event("evt-retry-due", "/input/user");
            bus.publish(original.clone()).unwrap();
            bus.dead_letter_for_handlers(
                original,
                EvaError::timeout("handler timeout"),
                vec![root_echo_binding()],
            )
            .unwrap();

            let report = run_scheduler_retry_tick_with_handler(
                &project(),
                &mut bus,
                &handler,
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
            assert_eq!(report.dispatched_events[0].ack_consumer, "root-agent");
            assert_eq!(
                bus.event_log_status(&EventId::parse("evt-retry-due:replay-1").unwrap()),
                Some(EventLogStatus::Acked)
            );

            let second = run_scheduler_retry_tick_with_handler(
                &project(),
                &mut bus,
                &handler,
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

            let report = run_scheduler_retry_tick_with_handler(
                &project(),
                &mut bus,
                &handler,
                SchedulerRetryTickOptions::default(),
            )
            .unwrap();

            assert!(report.dispatched_events.is_empty());
            assert_eq!(report.skipped_events[0].reason, "already_dispatched");
            assert_eq!(bus.log().records().len(), 2);
        }

        assert_eq!(invocations.load(Ordering::SeqCst), 1);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    /// 中文：验证 legacy 无 handler binding 的 replay 只能失败，绝不能被临时邮箱伪 ACK。
    fn scheduler_retry_tick_without_handler_binding_never_acks() {
        let root = test_root("missing-binding");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)).unwrap();
        let mut bus = DurableEventBus::open(backend.layout()).unwrap();
        let original = event("evt-retry-unbound", "/input/user");
        bus.publish(original.clone()).unwrap();
        bus.dead_letter(original, EvaError::timeout("handler timeout"))
            .unwrap();

        let report =
            run_scheduler_retry_tick(&project(), &mut bus, SchedulerRetryTickOptions::default())
                .unwrap();

        assert!(report.dispatched_events.is_empty());
        assert_eq!(report.failed_events.len(), 1);
        assert_eq!(report.failed_events[0].reason_kind, "not_found");
        assert_eq!(
            bus.event_log_status(&EventId::parse("evt-retry-unbound:replay-1").unwrap()),
            Some(EventLogStatus::Failed)
        );
        assert_eq!(bus.dead_letters().len(), 1);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    /// 中文：验证 handler 失败后重开可复用同一 replay，成功后才 ACK 且不再调用。
    fn scheduler_retry_handler_failure_resumes_same_replay_after_reopen() {
        let root = test_root("handler-resume");
        let invocations = Arc::new(AtomicUsize::new(0));
        let handler = CountingReplayHandler::new(Arc::clone(&invocations), true);

        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)).unwrap();
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();
            let original = event("evt-retry-resume", "/input/user");
            bus.publish(original.clone()).unwrap();
            bus.dead_letter_for_handlers(
                original,
                EvaError::timeout("handler timeout"),
                vec![root_echo_binding()],
            )
            .unwrap();

            let first = run_scheduler_retry_tick_with_handler(
                &project(),
                &mut bus,
                &handler,
                SchedulerRetryTickOptions::default(),
            )
            .unwrap();
            assert_eq!(first.failed_events.len(), 1);
            assert_eq!(
                first.failed_events[0].replay_event_id,
                "evt-retry-resume:replay-1"
            );
            assert_eq!(
                bus.event_log_status(&EventId::parse("evt-retry-resume:replay-1").unwrap()),
                Some(EventLogStatus::Failed)
            );
        }

        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)).unwrap();
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();
            let resumed = run_scheduler_retry_tick_with_handler(
                &project(),
                &mut bus,
                &handler,
                SchedulerRetryTickOptions::default(),
            )
            .unwrap();
            assert_eq!(resumed.dispatched_events.len(), 1);
            assert_eq!(
                resumed.dispatched_events[0].replay_event_id,
                "evt-retry-resume:replay-1"
            );
            assert_eq!(bus.dead_letters()[0].replay_count, 1);

            let final_tick = run_scheduler_retry_tick_with_handler(
                &project(),
                &mut bus,
                &handler,
                SchedulerRetryTickOptions::default(),
            )
            .unwrap();
            assert_eq!(final_tick.skipped_events[0].reason, "already_dispatched");
        }

        assert_eq!(invocations.load(Ordering::SeqCst), 2);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    /// 中文：验证 append 后、handler 前重启会继续同一 replay，而不是永久 in-flight。
    fn scheduler_retry_resumes_preexisting_appended_replay() {
        let root = test_root("appended-resume");
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)).unwrap();
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();
            let original = event("evt-retry-appended", "/input/user");
            bus.publish(original.clone()).unwrap();
            bus.dead_letter_for_handlers(
                original.clone(),
                EvaError::timeout("handler timeout"),
                vec![root_echo_binding()],
            )
            .unwrap();
            let receipt = bus.redrive_dead_letter(original.event_id()).unwrap();
            assert_eq!(receipt.event_id.as_str(), "evt-retry-appended:replay-1");
            assert_eq!(
                bus.event_log_status(&receipt.event_id),
                Some(EventLogStatus::Appended)
            );
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)).unwrap();
        let mut bus = DurableEventBus::open(backend.layout()).unwrap();
        let handler = CountingReplayHandler::new(Arc::clone(&calls), false);
        let report = run_scheduler_retry_tick_with_handler(
            &project(),
            &mut bus,
            &handler,
            SchedulerRetryTickOptions::default(),
        )
        .unwrap();

        assert_eq!(report.dispatched_events.len(), 1);
        assert_eq!(
            report.dispatched_events[0].replay_event_id,
            "evt-retry-appended:replay-1"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(bus.dead_letters()[0].replay_count, 1);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    /// 中文：验证 fanout binding 按路由顺序全部成功才 ACK，错配时零调用并失败关闭。
    fn scheduler_retry_fanout_requires_exact_ordered_handler_plan() {
        let root = test_root("fanout-plan");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)).unwrap();
        let mut bus = DurableEventBus::open(backend.layout()).unwrap();
        let original = event("evt-retry-fanout", "/sys/route-a/route-aa");
        bus.publish(original.clone()).unwrap();
        bus.dead_letter_for_handlers(
            original,
            EvaError::timeout("handler timeout"),
            vec![
                ReplayHandlerBinding::new("runtime.echo", AgentId::parse("agent-a11").unwrap())
                    .unwrap(),
                ReplayHandlerBinding::new("runtime.echo", AgentId::parse("agent-a12").unwrap())
                    .unwrap(),
            ],
        )
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let handler = CountingReplayHandler::new(Arc::clone(&calls), false);

        let report = run_scheduler_retry_tick_with_handler(
            &project(),
            &mut bus,
            &handler,
            SchedulerRetryTickOptions::default(),
        )
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(report.dispatched_events.len(), 1);
        assert_eq!(report.dispatched_events[0].deliveries.len(), 2);
        assert_eq!(
            report.dispatched_events[0].ack_consumer,
            SCHEDULER_RETRY_CONSUMER
        );

        let mismatch_root = test_root("fanout-mismatch");
        let mismatch_backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(&mismatch_root))
                .unwrap();
        let mut mismatch_bus = DurableEventBus::open(mismatch_backend.layout()).unwrap();
        let mismatch = event("evt-retry-fanout-mismatch", "/sys/route-a/route-aa");
        mismatch_bus.publish(mismatch.clone()).unwrap();
        mismatch_bus
            .dead_letter_for_handlers(
                mismatch,
                EvaError::timeout("handler timeout"),
                vec![root_echo_binding()],
            )
            .unwrap();
        let mismatch_calls = Arc::new(AtomicUsize::new(0));
        let mismatch_handler = CountingReplayHandler::new(Arc::clone(&mismatch_calls), false);
        let mismatch_report = run_scheduler_retry_tick_with_handler(
            &project(),
            &mut mismatch_bus,
            &mismatch_handler,
            SchedulerRetryTickOptions::default(),
        )
        .unwrap();
        assert_eq!(mismatch_calls.load(Ordering::SeqCst), 0);
        assert_eq!(mismatch_report.failed_events.len(), 1);
        assert_eq!(mismatch_report.failed_events[0].reason_kind, "conflict");

        fs::remove_dir_all(root).ok();
        fs::remove_dir_all(mismatch_root).ok();
    }

    #[test]
    /// 中文：验证 fanout 的成功 delivery 已持久 checkpoint，其他目标重试时不会重复执行。
    fn scheduler_retry_fanout_retries_only_failed_durable_delivery() {
        let root = test_root("fanout-durable-checkpoint");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let store = FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
            .unwrap();
        let observer = store.clone();
        let first_calls = Arc::new(AtomicUsize::new(0));
        let second_calls = Arc::new(AtomicUsize::new(0));
        let first_handler_calls = Arc::clone(&first_calls);
        let second_handler_calls = Arc::clone(&second_calls);
        let mut registry = crate::TaskHandlerRegistry::new();
        registry
            .register(
                crate::TaskKind::parse("vendor.fanout-first").unwrap(),
                move |invocation: &crate::TaskHandlerInvocation<'_>| {
                    first_handler_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(crate::TaskHandlerResult::new(invocation.payload()))
                },
            )
            .unwrap();
        registry
            .register(
                crate::TaskKind::parse("vendor.fanout-second").unwrap(),
                move |invocation: &crate::TaskHandlerInvocation<'_>| {
                    if second_handler_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err(EvaError::timeout("second fanout delivery retries once"))
                    } else {
                        Ok(crate::TaskHandlerResult::new(invocation.payload()))
                    }
                },
            )
            .unwrap();
        let artifacts: Arc<dyn crate::TaskArtifactResolver> =
            Arc::new(eva_storage::InMemoryArtifactStore::new());
        let mut worker = crate::TaskWorkerRuntime::start(
            store,
            Arc::new(registry),
            artifacts,
            "fanout-checkpoint-owner",
        )
        .unwrap();
        let mut bus = DurableEventBus::open_with_writer(backend.layout(), writer).unwrap();
        let original = event("evt-retry-fanout-checkpoint", "/sys/route-a/route-aa");
        bus.publish(original.clone()).unwrap();
        bus.dead_letter_for_handlers(
            original,
            EvaError::timeout("handler timeout"),
            vec![
                ReplayHandlerBinding::new(
                    "vendor.fanout-first",
                    AgentId::parse("agent-a11").unwrap(),
                )
                .unwrap(),
                ReplayHandlerBinding::new(
                    "vendor.fanout-second",
                    AgentId::parse("agent-a12").unwrap(),
                )
                .unwrap(),
            ],
        )
        .unwrap();

        let initial = run_scheduler_retry_tick_with_handler(
            &project(),
            &mut bus,
            &worker,
            SchedulerRetryTickOptions::default(),
        )
        .unwrap();
        assert_eq!(initial.skipped_events[0].reason, "handler_pending");
        wait_for_replay_task_status(&observer, "vendor.fanout-first", "completed");
        wait_for_replay_task_status(&observer, "vendor.fanout-second", "timed_out");

        let failed = run_scheduler_retry_tick_with_handler(
            &project(),
            &mut bus,
            &worker,
            SchedulerRetryTickOptions::default(),
        )
        .unwrap();
        assert_eq!(failed.failed_events.len(), 1);
        wait_for_replay_task_status(&observer, "vendor.fanout-second", "completed");

        let completed = run_scheduler_retry_tick_with_handler(
            &project(),
            &mut bus,
            &worker,
            SchedulerRetryTickOptions::default(),
        )
        .unwrap();
        assert_eq!(completed.dispatched_events.len(), 1);
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_calls.load(Ordering::SeqCst), 2);
        worker.stop_and_join().unwrap();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scheduler_retry_waits_for_persisted_backoff_before_requeue() {
        let root = test_root("owned-replay-backoff");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let store = FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
            .unwrap();
        let observer = store.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = Arc::clone(&calls);
        let mut registry = crate::TaskHandlerRegistry::new();
        registry
            .register(
                crate::TaskKind::parse("vendor.retry-backoff").unwrap(),
                move |invocation: &crate::TaskHandlerInvocation<'_>| {
                    if handler_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err(EvaError::timeout("retry after durable backoff"))
                    } else {
                        Ok(crate::TaskHandlerResult::new(invocation.payload()))
                    }
                },
            )
            .unwrap();
        let artifacts: Arc<dyn crate::TaskArtifactResolver> =
            Arc::new(eva_storage::InMemoryArtifactStore::new());
        let mut worker = crate::TaskWorkerRuntime::start(
            store,
            Arc::new(registry),
            artifacts,
            "retry-backoff-owner",
        )
        .unwrap();
        let mut bus = DurableEventBus::open_with_writer(backend.layout(), writer).unwrap();
        let original = event("evt-retry-backoff", "/input/user");
        bus.publish(original.clone()).unwrap();
        bus.dead_letter_for_handlers(
            original.clone(),
            EvaError::timeout("handler timeout"),
            vec![ReplayHandlerBinding::new(
                "vendor.retry-backoff",
                AgentId::parse("root-agent").unwrap(),
            )
            .unwrap()],
        )
        .unwrap();
        bus.set_dead_letter_redrive_policy(
            original.event_id(),
            RedrivePolicy {
                retry_delay_ms: 1_000,
                next_attempt_after_ms: 100,
            },
        )
        .unwrap();

        let initial = run_scheduler_retry_tick_with_handler(
            &project(),
            &mut bus,
            &worker,
            SchedulerRetryTickOptions {
                redrive_ready_at_ms: 100,
                ..SchedulerRetryTickOptions::default()
            },
        )
        .unwrap();
        assert_eq!(initial.skipped_events[0].reason, "handler_pending");
        wait_for_replay_task_status(&observer, "vendor.retry-backoff", "timed_out");

        let failed = run_scheduler_retry_tick_with_handler(
            &project(),
            &mut bus,
            &worker,
            SchedulerRetryTickOptions {
                redrive_ready_at_ms: 110,
                ..SchedulerRetryTickOptions::default()
            },
        )
        .unwrap();
        assert_eq!(failed.failed_events.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let scheduled = observer
            .list_records()
            .unwrap()
            .into_iter()
            .find(|snapshot| {
                snapshot
                    .envelope
                    .as_ref()
                    .is_some_and(|envelope| envelope.kind == "vendor.retry-backoff")
            })
            .unwrap();
        assert_eq!(scheduled.status, "timed_out");
        assert_eq!(scheduled.retry_ready_at_ms, Some(1_110));

        let early = run_scheduler_retry_tick_with_handler(
            &project(),
            &mut bus,
            &worker,
            SchedulerRetryTickOptions {
                redrive_ready_at_ms: 1_109,
                ..SchedulerRetryTickOptions::default()
            },
        )
        .unwrap();
        assert_eq!(early.skipped_events[0].reason, "redrive_not_due");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let due = run_scheduler_retry_tick_with_handler(
            &project(),
            &mut bus,
            &worker,
            SchedulerRetryTickOptions {
                redrive_ready_at_ms: 1_110,
                ..SchedulerRetryTickOptions::default()
            },
        )
        .unwrap();
        assert_eq!(due.skipped_events[0].reason, "handler_pending");
        wait_for_replay_task_status(&observer, "vendor.retry-backoff", "completed");
        let completed = run_scheduler_retry_tick_with_handler(
            &project(),
            &mut bus,
            &worker,
            SchedulerRetryTickOptions {
                redrive_ready_at_ms: 1_110,
                ..SchedulerRetryTickOptions::default()
            },
        )
        .unwrap();
        assert_eq!(completed.dispatched_events.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        worker.stop_and_join().unwrap();
        fs::remove_dir_all(root).ok();
    }

    fn wait_for_replay_task_status(
        store: &FileSystemTaskStateStore,
        task_kind: &str,
        expected_status: &str,
    ) {
        let started = Instant::now();
        loop {
            let matched = store.list_records().unwrap().into_iter().any(|snapshot| {
                snapshot.status == expected_status
                    && snapshot
                        .envelope
                        .as_ref()
                        .is_some_and(|envelope| envelope.kind == task_kind)
            });
            if matched {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "replay task {task_kind} did not reach {expected_status}"
            );
            thread::sleep(Duration::from_millis(10));
        }
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
