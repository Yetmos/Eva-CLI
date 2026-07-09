//! V1.12.4 scheduler retry tick over durable dead-letter backoff.

use eva_config::{ProjectConfig, RouteDelivery};
use eva_core::{AgentId, EvaError, Event, EventId};
use eva_eventbus::{DurableEventBus, EventBus};
use eva_scheduler::{
    dispatch_retry_event, DeliveryMode, DeliveryPlan, MailboxRegistry, RoutingRule,
    SubscriptionTable,
};
use eva_storage::EventLogStatus;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "dispatch due durable dead-letter retries through scheduler";

const DEFAULT_MAILBOX_CAPACITY: usize = 256;
const SCHEDULER_RETRY_CONSUMER: &str = "scheduler-retry";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerRetryTickOptions {
    pub redrive_ready_at_ms: u64,
    pub mailbox_capacity: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerRetryTickReport {
    pub scanned_dead_letters: usize,
    pub due_dead_letters: usize,
    pub dispatched_events: Vec<SchedulerRetryDispatchedEvent>,
    pub failed_events: Vec<SchedulerRetryFailedEvent>,
    pub skipped_events: Vec<SchedulerRetrySkippedEvent>,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerRetryDispatchedEvent {
    pub event_id: String,
    pub replay_event_id: String,
    pub sequence: u64,
    pub topic: String,
    pub ack_consumer: String,
    pub deliveries: Vec<DeliveryPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerRetryFailedEvent {
    pub event_id: String,
    pub replay_event_id: String,
    pub sequence: u64,
    pub reason_kind: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerRetrySkippedEvent {
    pub event_id: String,
    pub reason: String,
}

impl Default for SchedulerRetryTickOptions {
    fn default() -> Self {
        Self {
            redrive_ready_at_ms: 0,
            mailbox_capacity: DEFAULT_MAILBOX_CAPACITY,
        }
    }
}

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

fn replay_event(bus: &DurableEventBus, event_id: &EventId) -> Result<Event, EvaError> {
    bus.event_log_record(event_id)
        .map(|record| record.event.clone())
        .ok_or_else(|| {
            EvaError::internal("redriven event was not persisted before scheduler dispatch")
                .with_context("event_id", event_id.as_str())
        })
}

fn skip_retry(report: &mut SchedulerRetryTickReport, event_id: &EventId, reason: &'static str) {
    report.skipped_events.push(SchedulerRetrySkippedEvent {
        event_id: event_id.as_str().to_owned(),
        reason: reason.to_owned(),
    });
}

fn scheduler_retry_consumer() -> Result<AgentId, EvaError> {
    AgentId::parse(SCHEDULER_RETRY_CONSUMER)
}

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

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

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

    fn project() -> ProjectConfig {
        load_project_config(workspace_root()).unwrap()
    }

    fn event(id: &str, topic: &str) -> Event {
        Event::new(
            EventId::parse(id).unwrap(),
            Topic::parse(topic).unwrap(),
            EventPayload::empty(),
        )
        .with_request_id(RequestId::parse("req-retry").unwrap())
    }

    #[test]
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
