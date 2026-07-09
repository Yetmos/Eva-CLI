//! V1.6.4 runtime crash recovery coordinator.

use eva_core::{EvaError, EventId};
use eva_eventbus::DurableEventBus;
use eva_observability::{AuditAction, AuditEvent, AuditOutcome, AuditSink, TraceFields};
use eva_scheduler::{decide_retry_backoff, RetryBackoffPolicy};
use eva_storage::{
    EventLogStatus, FileSystemTaskStateStore, ProviderProcessSnapshot, ProviderProcessTable,
    TaskStateReplaySnapshot, TaskStateSnapshot, TaskStateStore,
};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "runtime restart recovery over durable task evidence";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RuntimeRecoveryCoordinator;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeRecoveryOptions {
    pub redrive_ready_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRecoveryReport {
    pub scanned_tasks: usize,
    pub recovered_tasks: Vec<RecoveredTask>,
    pub unchanged_tasks: Vec<String>,
    pub recovered_snapshots: Vec<TaskStateSnapshot>,
    pub redriven_events: Vec<RecoveredEvent>,
    pub skipped_redrive_events: Vec<SkippedRedriveEvent>,
    pub scanned_provider_processes: usize,
    pub recovered_provider_processes: Vec<RecoveredProviderProcess>,
    pub unchanged_provider_processes: Vec<String>,
    pub provider_backoff_tasks: Vec<ProviderBackoffTask>,
    pub skipped_provider_tasks: Vec<SkippedProviderTask>,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredTask {
    pub task_id: String,
    pub previous_status: String,
    pub status: String,
    pub redrive_candidate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredEvent {
    pub task_id: String,
    pub event_id: String,
    pub replay_event_id: String,
    pub sequence: u64,
    pub topic: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedRedriveEvent {
    pub task_id: String,
    pub event_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredProviderProcess {
    pub session_id: String,
    pub provider_process_id: String,
    pub request_id: String,
    pub adapter_id: String,
    pub previous_health: String,
    pub health: String,
    pub task_id: String,
    pub task_status: Option<String>,
    pub retry_scheduled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderBackoffTask {
    pub task_id: String,
    pub session_id: String,
    pub next_attempt: u32,
    pub due_after_ms: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedProviderTask {
    pub task_id: String,
    pub session_id: String,
    pub reason: String,
}

impl RuntimeRecoveryCoordinator {
    pub fn recover_snapshots(&self, snapshots: Vec<TaskStateSnapshot>) -> RuntimeRecoveryReport {
        let scanned_tasks = snapshots.len();
        let mut recovered_tasks = Vec::new();
        let mut unchanged_tasks = Vec::new();
        let mut recovered_snapshots = Vec::new();

        for mut snapshot in snapshots {
            let previous_status = snapshot.status.clone();
            let Some(status) = recovery_status(&snapshot) else {
                unchanged_tasks.push(snapshot.task_id);
                continue;
            };
            let redrive_candidate = status == "recovering";
            snapshot.status = status.to_owned();
            snapshot.push_log(
                "warning",
                format!("runtime recovery marked {previous_status} task as {status} after restart"),
            );
            recovered_tasks.push(RecoveredTask {
                task_id: snapshot.task_id.clone(),
                previous_status,
                status: status.to_owned(),
                redrive_candidate,
            });
            recovered_snapshots.push(snapshot);
        }

        let audit = vec![
            format!("runtime.recovery:scanned:{scanned_tasks}"),
            format!("runtime.recovery:recovered:{}", recovered_tasks.len()),
            format!("runtime.recovery:unchanged:{}", unchanged_tasks.len()),
        ];

        RuntimeRecoveryReport {
            scanned_tasks,
            recovered_tasks,
            unchanged_tasks,
            recovered_snapshots,
            redriven_events: Vec::new(),
            skipped_redrive_events: Vec::new(),
            scanned_provider_processes: 0,
            recovered_provider_processes: Vec::new(),
            unchanged_provider_processes: Vec::new(),
            provider_backoff_tasks: Vec::new(),
            skipped_provider_tasks: Vec::new(),
            audit,
        }
    }

    pub fn recover_task_store(
        &self,
        store: &mut FileSystemTaskStateStore,
    ) -> Result<RuntimeRecoveryReport, EvaError> {
        let mut report = self.recover_snapshots(store.list_snapshots()?);
        persist_recovered_snapshots(store, &mut report)?;
        Ok(report)
    }

    pub fn recover_task_store_with_provider_processes(
        &self,
        store: &mut FileSystemTaskStateStore,
        process_table: &mut impl ProviderProcessTable,
    ) -> Result<RuntimeRecoveryReport, EvaError> {
        let mut report = self.recover_snapshots(store.list_snapshots()?);
        recover_provider_processes(&mut report, process_table)?;
        persist_recovered_snapshots(store, &mut report)?;
        Ok(report)
    }

    pub fn recover_task_store_with_audit(
        &self,
        store: &mut FileSystemTaskStateStore,
        audit_sink: &mut impl AuditSink,
        trace: TraceFields,
    ) -> Result<RuntimeRecoveryReport, EvaError> {
        let mut report = self.recover_task_store(store)?;
        self.record_recovery_audit(audit_sink, trace, &report)?;
        report
            .audit
            .push("runtime.recovery:audit_recorded".to_owned());
        Ok(report)
    }

    pub fn recover_task_store_with_redrive(
        &self,
        store: &mut FileSystemTaskStateStore,
        bus: &mut DurableEventBus,
        options: RuntimeRecoveryOptions,
    ) -> Result<RuntimeRecoveryReport, EvaError> {
        let mut report = self.recover_snapshots(store.list_snapshots()?);
        redrive_recovered_events(&mut report, bus, options)?;
        persist_recovered_snapshots(store, &mut report)?;
        Ok(report)
    }

    pub fn recover_task_store_with_redrive_and_audit(
        &self,
        store: &mut FileSystemTaskStateStore,
        bus: &mut DurableEventBus,
        audit_sink: &mut impl AuditSink,
        trace: TraceFields,
        options: RuntimeRecoveryOptions,
    ) -> Result<RuntimeRecoveryReport, EvaError> {
        let mut report = self.recover_task_store_with_redrive(store, bus, options)?;
        self.record_recovery_audit(audit_sink, trace, &report)?;
        report
            .audit
            .push("runtime.recovery:audit_recorded".to_owned());
        Ok(report)
    }

    pub fn record_recovery_audit(
        &self,
        audit_sink: &mut impl AuditSink,
        trace: TraceFields,
        report: &RuntimeRecoveryReport,
    ) -> Result<(), EvaError> {
        audit_sink.record(recovery_audit_event(trace, report))
    }
}

fn recovery_status(snapshot: &TaskStateSnapshot) -> Option<&'static str> {
    match snapshot.status.as_str() {
        "queued" | "running" | "cancelling" if !snapshot.dead_letters.is_empty() => {
            Some("recovering")
        }
        "queued" | "running" | "cancelling" => Some("interrupted"),
        _ => None,
    }
}

fn persist_recovered_snapshots(
    store: &mut FileSystemTaskStateStore,
    report: &mut RuntimeRecoveryReport,
) -> Result<(), EvaError> {
    for snapshot in &report.recovered_snapshots {
        store.write(snapshot)?;
    }
    report.audit.push(format!(
        "runtime.recovery:persisted:{}",
        report.recovered_snapshots.len()
    ));
    Ok(())
}

fn redrive_recovered_events(
    report: &mut RuntimeRecoveryReport,
    bus: &mut DurableEventBus,
    options: RuntimeRecoveryOptions,
) -> Result<(), EvaError> {
    let mut redriven_events = Vec::new();
    let mut skipped_redrive_events = Vec::new();

    for snapshot in &mut report.recovered_snapshots {
        if snapshot.status != "recovering" {
            continue;
        }

        let task_id = snapshot.task_id.clone();
        let dead_letters = snapshot.dead_letters.clone();
        for dead_letter in dead_letters {
            let event_id = match EventId::parse(&dead_letter.event_id) {
                Ok(event_id) => event_id,
                Err(_) => {
                    skip_redrive(
                        &mut skipped_redrive_events,
                        task_id.clone(),
                        dead_letter.event_id,
                        "invalid_event_id",
                    );
                    continue;
                }
            };

            let Some(record) = bus
                .dead_letters()
                .iter()
                .find(|record| record.event_id() == &event_id)
            else {
                skip_redrive(
                    &mut skipped_redrive_events,
                    task_id.clone(),
                    event_id.as_str().to_owned(),
                    "dead_letter_missing",
                );
                continue;
            };

            if record.redrive.next_attempt_after_ms > options.redrive_ready_at_ms {
                skip_redrive(
                    &mut skipped_redrive_events,
                    task_id.clone(),
                    event_id.as_str().to_owned(),
                    "redrive_not_due",
                );
                continue;
            }

            match bus.event_log_status(&event_id) {
                Some(EventLogStatus::Acked) => {
                    skip_redrive(
                        &mut skipped_redrive_events,
                        task_id.clone(),
                        event_id.as_str().to_owned(),
                        "already_acked",
                    );
                    continue;
                }
                Some(EventLogStatus::Appended | EventLogStatus::Failed) => {}
                None => {
                    skip_redrive(
                        &mut skipped_redrive_events,
                        task_id.clone(),
                        event_id.as_str().to_owned(),
                        "event_log_missing",
                    );
                    continue;
                }
            }
            if let Some(replay) = bus.latest_replay_record(&event_id) {
                let reason = match replay.status {
                    EventLogStatus::Acked => "already_redriven",
                    EventLogStatus::Appended => "replay_in_flight",
                    EventLogStatus::Failed => "replay_failed",
                };
                skip_redrive(
                    &mut skipped_redrive_events,
                    task_id.clone(),
                    event_id.as_str().to_owned(),
                    reason,
                );
                continue;
            }

            let receipt = bus.redrive_dead_letter(&event_id)?;
            snapshot.replayed_events.push(TaskStateReplaySnapshot {
                event_id: receipt.event_id.as_str().to_owned(),
                sequence: receipt.sequence,
                topic: receipt.topic.as_str().to_owned(),
            });
            snapshot.push_log(
                "info",
                format!(
                    "runtime recovery redrove dead-letter event {} as {}",
                    event_id.as_str(),
                    receipt.event_id.as_str()
                ),
            );
            redriven_events.push(RecoveredEvent {
                task_id: task_id.clone(),
                event_id: event_id.as_str().to_owned(),
                replay_event_id: receipt.event_id.as_str().to_owned(),
                sequence: receipt.sequence,
                topic: receipt.topic.as_str().to_owned(),
            });
        }
    }

    report.redriven_events.extend(redriven_events);
    report.skipped_redrive_events.extend(skipped_redrive_events);
    report.audit.push(format!(
        "runtime.recovery:redriven:{}",
        report.redriven_events.len()
    ));
    report.audit.push(format!(
        "runtime.recovery:redrive_skipped:{}",
        report.skipped_redrive_events.len()
    ));
    Ok(())
}

fn skip_redrive(
    skipped_redrive_events: &mut Vec<SkippedRedriveEvent>,
    task_id: String,
    event_id: String,
    reason: &'static str,
) {
    skipped_redrive_events.push(SkippedRedriveEvent {
        task_id,
        event_id,
        reason: reason.to_owned(),
    });
}

fn recovery_audit_event(trace: TraceFields, report: &RuntimeRecoveryReport) -> AuditEvent {
    AuditEvent::new(AuditAction::RuntimeRecovered, AuditOutcome::Ok, trace)
        .with_message("runtime recovery checkpoint completed")
        .with_field("scanned_tasks", report.scanned_tasks.to_string())
        .with_field("recovered_tasks", report.recovered_tasks.len().to_string())
        .with_field("unchanged_tasks", report.unchanged_tasks.len().to_string())
        .with_field("redriven_events", report.redriven_events.len().to_string())
        .with_field(
            "skipped_redrive_events",
            report.skipped_redrive_events.len().to_string(),
        )
        .with_field(
            "scanned_provider_processes",
            report.scanned_provider_processes.to_string(),
        )
        .with_field(
            "recovered_provider_processes",
            report.recovered_provider_processes.len().to_string(),
        )
}

fn recover_provider_processes(
    report: &mut RuntimeRecoveryReport,
    process_table: &mut impl ProviderProcessTable,
) -> Result<(), EvaError> {
    let processes = process_table.list()?;
    report.scanned_provider_processes = processes.len();

    for mut process in processes {
        if !process.active {
            report.unchanged_provider_processes.push(process.session_id);
            continue;
        }

        let previous_health = process.health.clone();
        let task_id = process.request_id.as_str().to_owned();
        let task_status = recover_provider_task(report, &process);
        let retry_scheduled = task_status
            .as_ref()
            .map(|status| status == "recovering")
            .unwrap_or(false)
            && report
                .provider_backoff_tasks
                .iter()
                .any(|entry| entry.session_id == process.session_id);
        process
            .mark_interrupted_after_restart("daemon restart interrupted active provider session");
        process_table.upsert(process.clone())?;

        report
            .recovered_provider_processes
            .push(RecoveredProviderProcess {
                session_id: process.session_id,
                provider_process_id: process.provider_process_id,
                request_id: process.request_id.as_str().to_owned(),
                adapter_id: process.adapter_id.as_str().to_owned(),
                previous_health,
                health: process.health,
                task_id,
                task_status,
                retry_scheduled,
            });
    }

    report.audit.push(format!(
        "runtime.recovery:provider_scanned:{}",
        report.scanned_provider_processes
    ));
    report.audit.push(format!(
        "runtime.recovery:provider_recovered:{}",
        report.recovered_provider_processes.len()
    ));
    report.audit.push(format!(
        "runtime.recovery:provider_unchanged:{}",
        report.unchanged_provider_processes.len()
    ));
    report.audit.push(format!(
        "runtime.recovery:provider_backoff:{}",
        report.provider_backoff_tasks.len()
    ));
    report.audit.push(format!(
        "runtime.recovery:provider_task_skipped:{}",
        report.skipped_provider_tasks.len()
    ));
    Ok(())
}

fn recover_provider_task(
    report: &mut RuntimeRecoveryReport,
    process: &ProviderProcessSnapshot,
) -> Option<String> {
    let task_id = process.request_id.as_str();
    let Some(snapshot) = report
        .recovered_snapshots
        .iter_mut()
        .find(|snapshot| snapshot.task_id == task_id)
    else {
        let reason = if report.unchanged_tasks.iter().any(|task| task == task_id) {
            "task_already_terminal"
        } else {
            "task_snapshot_missing"
        };
        report.skipped_provider_tasks.push(SkippedProviderTask {
            task_id: task_id.to_owned(),
            session_id: process.session_id.clone(),
            reason: reason.to_owned(),
        });
        return None;
    };

    let backoff = provider_backoff_decision(process, snapshot);
    if let Some(backoff) = backoff {
        snapshot.status = "recovering".to_owned();
        snapshot.interrupted_reason =
            Some("provider restart recovery scheduled scheduler backoff".to_owned());
        snapshot.push_log(
            "warning",
            format!(
                "provider recovery scheduled scheduler backoff for session {} after {}ms",
                process.session_id, backoff.due_after_ms
            ),
        );
        report.provider_backoff_tasks.push(backoff);
    } else {
        if snapshot.status != "recovering" {
            snapshot.status = "interrupted".to_owned();
            snapshot.interrupted_reason =
                Some("provider session interrupted by daemon restart".to_owned());
            snapshot.error_kind = Some("interrupted".to_owned());
            snapshot.error_message =
                Some("provider session interrupted by daemon restart".to_owned());
        }
        snapshot.push_log(
            "warning",
            format!(
                "provider recovery linked interrupted session {}",
                process.session_id
            ),
        );
    }
    Some(snapshot.status.clone())
}

fn provider_backoff_decision(
    process: &ProviderProcessSnapshot,
    snapshot: &TaskStateSnapshot,
) -> Option<ProviderBackoffTask> {
    if !provider_restart_policy_allows_backoff(&process.restart_policy) {
        return None;
    }
    let retry_backoff_ms = process.retry_backoff_ms?;
    let max_attempts = snapshot.retry_max_attempts.try_into().unwrap_or(u32::MAX);
    let attempts = snapshot.attempts.try_into().unwrap_or(u32::MAX);
    let decision = decide_retry_backoff(
        true,
        attempts,
        RetryBackoffPolicy {
            max_attempts,
            backoff_ms: retry_backoff_ms,
        },
    );
    if !decision.enqueue {
        return None;
    }
    Some(ProviderBackoffTask {
        task_id: snapshot.task_id.clone(),
        session_id: process.session_id.clone(),
        next_attempt: decision.next_attempt,
        due_after_ms: decision.due_after_ms.unwrap_or(retry_backoff_ms),
        reason: decision.reason,
    })
}

fn provider_restart_policy_allows_backoff(policy: &str) -> bool {
    matches!(policy, "scheduler_backoff" | "retry_backoff" | "retryable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::{AgentId, EvaError, Event, EventId, EventPayload, Topic};
    use eva_eventbus::{DurableEventBus, EventBus, RedrivePolicy};
    use eva_observability::{SpanId, TraceFields};
    use eva_storage::{
        DurableBackendOptions, FileSystemAuditSink, FileSystemDurableBackend,
        FileSystemProviderProcessTable, ProviderProcessTable, TaskStateDeadLetterSnapshot,
        TaskStateLogSnapshot,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn recovery_marks_incomplete_tasks_without_touching_terminal_tasks() {
        let coordinator = RuntimeRecoveryCoordinator;
        let report = coordinator.recover_snapshots(vec![
            snapshot("req-recovery-1", "running"),
            snapshot("req-recovery-2", "completed"),
        ]);

        assert_eq!(report.scanned_tasks, 2);
        assert_eq!(report.recovered_tasks.len(), 1);
        assert_eq!(report.recovered_tasks[0].task_id, "req-recovery-1");
        assert_eq!(report.recovered_tasks[0].status, "interrupted");
        assert_eq!(report.unchanged_tasks, vec!["req-recovery-2"]);
        assert_eq!(report.recovered_snapshots[0].status, "interrupted");
        assert!(report.recovered_snapshots[0]
            .logs
            .iter()
            .any(|entry| entry.message.contains("after restart")));
    }

    #[test]
    fn recovery_marks_cancelling_tasks_as_interrupted() {
        let coordinator = RuntimeRecoveryCoordinator;
        let report =
            coordinator.recover_snapshots(vec![snapshot("req-recovery-cancelling", "cancelling")]);

        assert_eq!(report.scanned_tasks, 1);
        assert_eq!(report.recovered_tasks[0].task_id, "req-recovery-cancelling");
        assert_eq!(report.recovered_tasks[0].status, "interrupted");
        assert_eq!(report.recovered_snapshots[0].status, "interrupted");
    }

    #[test]
    fn recovery_marks_dead_letter_tasks_as_recovering() {
        let coordinator = RuntimeRecoveryCoordinator;
        let mut pending = snapshot("req-recovery-redrive", "running");
        pending.dead_letters.push(TaskStateDeadLetterSnapshot {
            event_id: "evt-recovery-1".to_owned(),
            topic: "/input/user".to_owned(),
            reason_kind: "timeout".to_owned(),
            reason: "agent timed out".to_owned(),
            replay_count: 0,
        });

        let report = coordinator.recover_snapshots(vec![pending]);

        assert_eq!(report.recovered_tasks[0].status, "recovering");
        assert!(report.recovered_tasks[0].redrive_candidate);
        assert_eq!(report.recovered_snapshots[0].status, "recovering");
    }

    #[test]
    fn recovery_persists_task_store_updates() {
        let root = test_root("store");
        let mut store = FileSystemTaskStateStore::new(root.path());
        store
            .write(&snapshot("req-recovery-store-1", "running"))
            .unwrap();
        store
            .write(&snapshot("req-recovery-store-2", "completed"))
            .unwrap();

        let report = RuntimeRecoveryCoordinator
            .recover_task_store(&mut store)
            .unwrap();
        let recovered = store.read(Some("req-recovery-store-1")).unwrap();
        let completed = store.read(Some("req-recovery-store-2")).unwrap();

        assert_eq!(report.recovered_tasks.len(), 1);
        assert_eq!(recovered.status, "interrupted");
        assert_eq!(completed.status, "completed");
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "runtime.recovery:persisted:1"));
    }

    #[test]
    fn recovery_audit_covers_clean_start_smoke() {
        let root = test_root("audit-clean-start");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let mut store = FileSystemTaskStateStore::from_durable_layout(backend.layout());
        let mut audit = FileSystemAuditSink::open(backend.layout()).unwrap();
        let trace = TraceFields::default()
            .with_span_id(SpanId::parse("span-recovery-clean-start").unwrap());

        let report = RuntimeRecoveryCoordinator
            .recover_task_store_with_audit(&mut store, &mut audit, trace)
            .unwrap();
        let reopened = FileSystemAuditSink::open(backend.layout()).unwrap();
        let records = reopened.query_by_trace_id("span-recovery-clean-start");

        assert_eq!(report.scanned_tasks, 0);
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "runtime.recovery:audit_recorded"));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].action, "runtime.recovered");
        assert_eq!(records[0].outcome, "ok");
        assert!(records[0]
            .fields
            .iter()
            .any(|field| field == &("scanned_tasks".to_owned(), "0".to_owned())));
    }

    #[test]
    fn recovery_redrives_unacked_dead_letters_and_records_task_replay() {
        let root = test_root("redrive-durable");
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut store = FileSystemTaskStateStore::from_durable_layout(backend.layout());
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();
            let event = event("evt-recovery-redrive-1");

            bus.publish(event.clone()).unwrap();
            bus.dead_letter(event, EvaError::timeout("handler timeout"))
                .unwrap();
            let mut pending = snapshot("req-recovery-redrive-store", "running");
            pending
                .dead_letters
                .push(dead_letter("evt-recovery-redrive-1"));
            store.write(&pending).unwrap();
        }

        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut store = FileSystemTaskStateStore::from_durable_layout(backend.layout());
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();

            let report = RuntimeRecoveryCoordinator
                .recover_task_store_with_redrive(
                    &mut store,
                    &mut bus,
                    RuntimeRecoveryOptions::default(),
                )
                .unwrap();
            let recovered = store.read(Some("req-recovery-redrive-store")).unwrap();

            assert_eq!(report.redriven_events.len(), 1);
            assert_eq!(
                report.redriven_events[0].replay_event_id,
                "evt-recovery-redrive-1:replay-1"
            );
            assert!(report.skipped_redrive_events.is_empty());
            assert_eq!(recovered.status, "recovering");
            assert_eq!(recovered.replayed_events.len(), 1);
            assert_eq!(
                recovered.replayed_events[0].event_id,
                "evt-recovery-redrive-1:replay-1"
            );
            assert_eq!(bus.dead_letters()[0].replay_count, 1);
            assert_eq!(bus.log().records().len(), 2);
            assert_eq!(
                bus.log().records()[1].event.event_id().as_str(),
                "evt-recovery-redrive-1:replay-1"
            );
        }
    }

    #[test]
    fn recovery_audit_covers_restart_redrive_smoke() {
        let root = test_root("audit-restart-redrive");
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut store = FileSystemTaskStateStore::from_durable_layout(backend.layout());
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();
            let event = event("evt-recovery-audit-1");

            bus.publish(event.clone()).unwrap();
            bus.dead_letter(event, EvaError::timeout("handler timeout"))
                .unwrap();
            let mut pending = snapshot("req-recovery-audit-store", "running");
            pending
                .dead_letters
                .push(dead_letter("evt-recovery-audit-1"));
            store.write(&pending).unwrap();
        }

        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut store = FileSystemTaskStateStore::from_durable_layout(backend.layout());
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();
            let mut audit = FileSystemAuditSink::open(backend.layout()).unwrap();
            let trace = TraceFields::default()
                .with_span_id(SpanId::parse("span-recovery-redrive").unwrap());

            let report = RuntimeRecoveryCoordinator
                .recover_task_store_with_redrive_and_audit(
                    &mut store,
                    &mut bus,
                    &mut audit,
                    trace,
                    RuntimeRecoveryOptions::default(),
                )
                .unwrap();
            let reopened = FileSystemAuditSink::open(backend.layout()).unwrap();
            let records = reopened.query_by_trace_id("span-recovery-redrive");

            assert_eq!(report.redriven_events.len(), 1);
            assert!(report
                .audit
                .iter()
                .any(|entry| entry == "runtime.recovery:audit_recorded"));
            assert_eq!(records.len(), 1);
            assert!(records[0]
                .fields
                .iter()
                .any(|field| field == &("redriven_events".to_owned(), "1".to_owned())));
        }
    }

    #[test]
    fn recovery_skips_redrive_for_acked_original_events() {
        let root = test_root("redrive-acked");
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut store = FileSystemTaskStateStore::from_durable_layout(backend.layout());
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();
            let event = event("evt-recovery-acked-1");

            bus.publish(event.clone()).unwrap();
            bus.dead_letter(event.clone(), EvaError::timeout("handler timeout"))
                .unwrap();
            bus.ack(event.event_id(), AgentId::parse("agent-a").unwrap())
                .unwrap();
            let mut pending = snapshot("req-recovery-acked-store", "running");
            pending
                .dead_letters
                .push(dead_letter("evt-recovery-acked-1"));
            store.write(&pending).unwrap();
        }

        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut store = FileSystemTaskStateStore::from_durable_layout(backend.layout());
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();

            let report = RuntimeRecoveryCoordinator
                .recover_task_store_with_redrive(
                    &mut store,
                    &mut bus,
                    RuntimeRecoveryOptions::default(),
                )
                .unwrap();
            let recovered = store.read(Some("req-recovery-acked-store")).unwrap();

            assert!(report.redriven_events.is_empty());
            assert_eq!(report.skipped_redrive_events.len(), 1);
            assert_eq!(report.skipped_redrive_events[0].reason, "already_acked");
            assert!(recovered.replayed_events.is_empty());
            assert_eq!(bus.log().records().len(), 1);
        }
    }

    #[test]
    fn recovery_skips_redrive_until_policy_is_due() {
        let root = test_root("redrive-policy");
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut store = FileSystemTaskStateStore::from_durable_layout(backend.layout());
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();
            let event = event("evt-recovery-policy-1");

            bus.publish(event.clone()).unwrap();
            bus.dead_letter(event.clone(), EvaError::timeout("handler timeout"))
                .unwrap();
            bus.set_dead_letter_redrive_policy(
                event.event_id(),
                RedrivePolicy {
                    retry_delay_ms: 5_000,
                    next_attempt_after_ms: 5_000,
                },
            )
            .unwrap();
            let mut pending = snapshot("req-recovery-policy-store", "running");
            pending
                .dead_letters
                .push(dead_letter("evt-recovery-policy-1"));
            store.write(&pending).unwrap();
        }

        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut store = FileSystemTaskStateStore::from_durable_layout(backend.layout());
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();

            let report = RuntimeRecoveryCoordinator
                .recover_task_store_with_redrive(
                    &mut store,
                    &mut bus,
                    RuntimeRecoveryOptions {
                        redrive_ready_at_ms: 1_000,
                    },
                )
                .unwrap();

            assert!(report.redriven_events.is_empty());
            assert_eq!(report.skipped_redrive_events.len(), 1);
            assert_eq!(report.skipped_redrive_events[0].reason, "redrive_not_due");
            assert_eq!(bus.dead_letters()[0].replay_count, 0);
            assert_eq!(bus.log().records().len(), 1);
        }
    }

    #[test]
    fn recovery_skips_dead_letter_after_scheduler_retry_dispatched_replay() {
        let root = test_root("redrive-already-dispatched");
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut store = FileSystemTaskStateStore::from_durable_layout(backend.layout());
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();
            let event = event("evt-recovery-already-redriven");

            bus.publish(event.clone()).unwrap();
            bus.dead_letter(event.clone(), EvaError::timeout("handler timeout"))
                .unwrap();
            let receipt = bus.redrive_dead_letter(event.event_id()).unwrap();
            bus.ack(
                &receipt.event_id,
                AgentId::parse("scheduler-retry").unwrap(),
            )
            .unwrap();
            let mut pending = snapshot("req-recovery-already-redriven", "running");
            pending
                .dead_letters
                .push(dead_letter("evt-recovery-already-redriven"));
            store.write(&pending).unwrap();
        }

        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut store = FileSystemTaskStateStore::from_durable_layout(backend.layout());
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();

            let report = RuntimeRecoveryCoordinator
                .recover_task_store_with_redrive(
                    &mut store,
                    &mut bus,
                    RuntimeRecoveryOptions::default(),
                )
                .unwrap();
            let recovered = store.read(Some("req-recovery-already-redriven")).unwrap();

            assert!(report.redriven_events.is_empty());
            assert_eq!(report.skipped_redrive_events.len(), 1);
            assert_eq!(report.skipped_redrive_events[0].reason, "already_redriven");
            assert!(recovered.replayed_events.is_empty());
            assert_eq!(bus.dead_letters()[0].replay_count, 1);
            assert_eq!(bus.log().records().len(), 2);
        }
    }

    #[test]
    fn recovery_reports_corrupt_task_store_smoke() {
        let root = test_root("corrupt-task-store");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        fs::write(
            backend.layout().task_dir.join("corrupt.task"),
            "task_id=req-corrupt\nstatus=running\nattempts=not-a-number\n",
        )
        .unwrap();
        let mut store = FileSystemTaskStateStore::from_durable_layout(backend.layout());

        let error = RuntimeRecoveryCoordinator
            .recover_task_store(&mut store)
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
    }

    #[test]
    fn recovery_interrupts_active_provider_process_and_preserves_task() {
        let root = test_root("provider-interrupted");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let mut store = FileSystemTaskStateStore::from_durable_layout(backend.layout());
        let mut processes = FileSystemProviderProcessTable::from_durable_layout(backend.layout());
        let mut task = snapshot("req-provider-recovery-interrupted", "running");
        task.retry_max_attempts = 3;
        store.write(&task).unwrap();
        processes
            .upsert(provider_process(
                "session-provider-recovery-interrupted",
                "req-provider-recovery-interrupted",
                "none",
                None,
            ))
            .unwrap();

        let report = RuntimeRecoveryCoordinator
            .recover_task_store_with_provider_processes(&mut store, &mut processes)
            .unwrap();
        let recovered_task = store
            .read(Some("req-provider-recovery-interrupted"))
            .unwrap();
        let recovered_process = processes
            .read("session-provider-recovery-interrupted")
            .unwrap();

        assert_eq!(report.scanned_provider_processes, 1);
        assert_eq!(report.recovered_provider_processes.len(), 1);
        assert_eq!(
            report.recovered_provider_processes[0]
                .task_status
                .as_deref(),
            Some("interrupted")
        );
        assert!(report.provider_backoff_tasks.is_empty());
        assert_eq!(recovered_task.status, "interrupted");
        assert_eq!(
            recovered_task.interrupted_reason.as_deref(),
            Some("provider session interrupted by daemon restart")
        );
        assert!(!recovered_process.active);
        assert_eq!(recovered_process.health, "interrupted");
        assert!(recovered_process
            .audit
            .iter()
            .any(|entry| entry == "provider.recovery:restart_scan"));
    }

    #[test]
    fn recovery_schedules_provider_backoff_only_when_restart_policy_allows_it() {
        let root = test_root("provider-backoff");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let mut store = FileSystemTaskStateStore::from_durable_layout(backend.layout());
        let mut processes = FileSystemProviderProcessTable::from_durable_layout(backend.layout());
        let mut task = snapshot("req-provider-recovery-backoff", "running");
        task.attempts = 1;
        task.retry_max_attempts = 3;
        store.write(&task).unwrap();
        processes
            .upsert(provider_process(
                "session-provider-recovery-backoff",
                "req-provider-recovery-backoff",
                "scheduler_backoff",
                Some(2500),
            ))
            .unwrap();

        let report = RuntimeRecoveryCoordinator
            .recover_task_store_with_provider_processes(&mut store, &mut processes)
            .unwrap();
        let recovered_task = store.read(Some("req-provider-recovery-backoff")).unwrap();

        assert_eq!(report.provider_backoff_tasks.len(), 1);
        assert_eq!(
            report.provider_backoff_tasks[0].task_id,
            "req-provider-recovery-backoff"
        );
        assert_eq!(report.provider_backoff_tasks[0].next_attempt, 2);
        assert_eq!(report.provider_backoff_tasks[0].due_after_ms, 2500);
        assert_eq!(
            report.recovered_provider_processes[0]
                .task_status
                .as_deref(),
            Some("recovering")
        );
        assert!(report.recovered_provider_processes[0].retry_scheduled);
        assert_eq!(recovered_task.status, "recovering");
        assert_eq!(
            recovered_task.interrupted_reason.as_deref(),
            Some("provider restart recovery scheduled scheduler backoff")
        );
    }

    fn snapshot(task_id: &str, status: &str) -> TaskStateSnapshot {
        TaskStateSnapshot {
            task_id: task_id.to_owned(),
            status: status.to_owned(),
            attempts: 1,
            retry_max_attempts: 2,
            cancel_requested: false,
            cancel_accepted: false,
            cancel_reason: None,
            heartbeat_at_ms: None,
            deadline_at_ms: None,
            cancel_token: None,
            interrupted_reason: None,
            error_kind: None,
            error_message: None,
            logs: vec![TaskStateLogSnapshot {
                sequence: 1,
                level: "info".to_owned(),
                message: "event accepted".to_owned(),
            }],
            dead_letters: Vec::new(),
            replayed_events: Vec::new(),
        }
    }

    fn dead_letter(event_id: &str) -> TaskStateDeadLetterSnapshot {
        TaskStateDeadLetterSnapshot {
            event_id: event_id.to_owned(),
            topic: "/input/user".to_owned(),
            reason_kind: "timeout".to_owned(),
            reason: "handler timeout".to_owned(),
            replay_count: 0,
        }
    }

    fn event(id: &str) -> Event {
        Event::new(
            EventId::parse(id).unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::text("hello"),
        )
    }

    fn provider_process(
        session_id: &str,
        request_id: &str,
        restart_policy: &str,
        retry_backoff_ms: Option<u64>,
    ) -> ProviderProcessSnapshot {
        let mut snapshot = ProviderProcessSnapshot::running(
            session_id,
            format!("proc-{session_id}"),
            eva_core::RequestId::parse(request_id).unwrap(),
            eva_core::AdapterId::parse("stdio-test").unwrap(),
            eva_core::CapabilityName::parse("repo.analyze").unwrap(),
            "stdio",
            "fnv64:0123456789abcdef",
            "stdio-runner --once",
            restart_policy,
        );
        snapshot.retry_backoff_ms = retry_backoff_ms;
        snapshot
    }

    struct TestRoot {
        path: PathBuf,
    }

    impl TestRoot {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn test_root(name: &str) -> TestRoot {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        TestRoot {
            path: std::env::temp_dir().join(format!(
                "eva-runtime-recovery-{name}-{}-{now}",
                std::process::id()
            )),
        }
    }
}
