//! 根据持久化任务、事件日志和提供者进程快照恢复守护进程重启后的可判定状态。
//!
//! 恢复不会假定崩溃前的内存执行仍可继续：非终态任务被标记为中断，带死信的任务进入可
//! 重驱态，活动提供者进程统一标记为重启中断。死信重驱在发布前检查原事件确认状态、到期
//! 时间和最新 replay 记录，以事件日志作为跨重启幂等边界。
//! V1.6.4 runtime crash recovery coordinator.

use eva_core::{EvaError, EventId};
use eva_eventbus::DurableEventBus;
use eva_observability::{AuditAction, AuditEvent, AuditOutcome, AuditSink, TraceFields};
use eva_scheduler::{decide_retry_backoff, RetryBackoffPolicy};
use eva_storage::{
    EventLogStatus, FileSystemTaskStateStore, ProviderProcessSnapshot, ProviderProcessTable,
    TaskStateReplaySnapshot, TaskStateSnapshot, TaskStateStore,
};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "runtime restart recovery over durable task evidence";

/// 无状态恢复协调器；所有恢复事实均来自调用方提供的持久化存储。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RuntimeRecoveryCoordinator;

/// 定义本轮恢复判定死信是否到期的逻辑时间。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeRecoveryOptions {
    /// 只有 `next_attempt_after_ms` 不晚于该时间的死信才允许重驱。
    pub redrive_ready_at_ms: u64,
}

/// 汇总任务、死信、提供者进程及退避计划的恢复决定和审计证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRecoveryReport {
    /// 记录 `scanned_tasks` 字段对应的值。
    pub scanned_tasks: usize,
    /// 记录 `recovered_tasks` 字段对应的值。
    pub recovered_tasks: Vec<RecoveredTask>,
    /// 记录 `unchanged_tasks` 字段对应的值。
    pub unchanged_tasks: Vec<String>,
    /// 记录 `recovered_snapshots` 字段对应的值。
    pub recovered_snapshots: Vec<TaskStateSnapshot>,
    /// 记录 `redriven_events` 字段对应的值。
    pub redriven_events: Vec<RecoveredEvent>,
    /// 记录 `skipped_redrive_events` 字段对应的值。
    pub skipped_redrive_events: Vec<SkippedRedriveEvent>,
    /// 记录 `scanned_provider_processes` 字段对应的值。
    pub scanned_provider_processes: usize,
    /// 记录 `recovered_provider_processes` 字段对应的值。
    pub recovered_provider_processes: Vec<RecoveredProviderProcess>,
    /// 记录 `unchanged_provider_processes` 字段对应的值。
    pub unchanged_provider_processes: Vec<String>,
    /// 记录 `provider_backoff_tasks` 字段对应的值。
    pub provider_backoff_tasks: Vec<ProviderBackoffTask>,
    /// 记录 `skipped_provider_tasks` 字段对应的值。
    pub skipped_provider_tasks: Vec<SkippedProviderTask>,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

/// 记录一个非终态任务在重启前后的状态转换。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredTask {
    /// 记录 `task_id` 字段对应的值。
    pub task_id: String,
    /// 记录 `previous_status` 字段对应的值。
    pub previous_status: String,
    /// 记录 `status` 字段对应的值。
    pub status: String,
    /// 表示任务因持有死信进入 `recovering`，但不保证本轮确实完成了重驱。
    pub redrive_candidate: bool,
}

/// 记录一次已成功写入持久化事件总线的死信重驱。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredEvent {
    /// 记录 `task_id` 字段对应的值。
    pub task_id: String,
    /// 记录 `event_id` 字段对应的值。
    pub event_id: String,
    /// 记录 `replay_event_id` 字段对应的值。
    pub replay_event_id: String,
    /// 记录 `sequence` 字段对应的值。
    pub sequence: u64,
    /// 记录 `topic` 字段对应的值。
    pub topic: String,
}

/// 表示 `SkippedRedriveEvent` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedRedriveEvent {
    /// 记录 `task_id` 字段对应的值。
    pub task_id: String,
    /// 记录 `event_id` 字段对应的值。
    pub event_id: String,
    /// 记录 `reason` 字段对应的值。
    pub reason: String,
}

/// 表示 `RecoveredProviderProcess` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredProviderProcess {
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `provider_process_id` 字段对应的值。
    pub provider_process_id: String,
    /// 记录 `request_id` 字段对应的值。
    pub request_id: String,
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: String,
    /// 记录 `previous_health` 字段对应的值。
    pub previous_health: String,
    /// 记录 `health` 字段对应的值。
    pub health: String,
    /// 记录 `task_id` 字段对应的值。
    pub task_id: String,
    /// 记录 `task_status` 字段对应的值。
    pub task_status: Option<String>,
    /// 记录 `retry_scheduled` 字段对应的值。
    pub retry_scheduled: bool,
}

/// 描述因提供者会话中断而交给调度器延后重试的任务决定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderBackoffTask {
    /// 记录 `task_id` 字段对应的值。
    pub task_id: String,
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `next_attempt` 字段对应的值。
    pub next_attempt: u32,
    /// 记录 `due_after_ms` 字段对应的值。
    pub due_after_ms: u64,
    /// 记录 `reason` 字段对应的值。
    pub reason: String,
}

/// 表示 `SkippedProviderTask` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedProviderTask {
    /// 记录 `task_id` 字段对应的值。
    pub task_id: String,
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `reason` 字段对应的值。
    pub reason: String,
}

/// 为相关类型实现其约定的行为与方法。
impl RuntimeRecoveryCoordinator {
    /// 仅在内存中分类并转换任务快照；终态任务保持不变，也不在此方法落盘或重驱。
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
            // 只有带持久化死信的非终态任务具备重驱证据；其他非终态任务只能判为中断。
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

    /// 执行 `recover_task_store` 对应的恢复或重驱流程。
    pub fn recover_task_store(
        &self,
        store: &mut FileSystemTaskStateStore,
    ) -> Result<RuntimeRecoveryReport, EvaError> {
        let mut report = self.recover_snapshots(store.list_snapshots()?);
        persist_recovered_snapshots(store, &mut report)?;
        Ok(report)
    }

    /// 先关联并中断活动提供者进程，再一次性持久化更新后的任务快照。
    ///
    /// 此顺序使提供者退避决定能够写进同一任务快照；若进程表更新失败，任务快照尚未落盘，
    /// 调用方可在下次启动重新执行恢复。
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

    /// 执行 `recover_task_store_with_audit` 对应的恢复或重驱流程。
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

    /// 先依据持久化事件日志执行幂等重驱，再保存包含 replay 证据的任务快照。
    ///
    /// 若事件已发布但快照写入失败，下次恢复会通过最新 replay 记录跳过重复发布，随后报告
    /// `already_redriven` 或相应在途状态。
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

    /// 执行 `recover_task_store_with_redrive_and_audit` 对应的恢复或重驱流程。
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

    /// 登记 `record_recovery_audit` 对应的数据或状态。
    pub fn record_recovery_audit(
        &self,
        audit_sink: &mut impl AuditSink,
        trace: TraceFields,
        report: &RuntimeRecoveryReport,
    ) -> Result<(), EvaError> {
        audit_sink.record(recovery_audit_event(trace, report))
    }
}

/// 只转换崩溃时可能仍在推进的状态；完成、失败等终态返回 `None` 并保持原样。
fn recovery_status(snapshot: &TaskStateSnapshot) -> Option<&'static str> {
    match snapshot.status.as_str() {
        "queued" | "running" | "cancelling" if !snapshot.dead_letters.is_empty() => {
            Some("recovering")
        }
        "queued" | "running" | "cancelling" => Some("interrupted"),
        _ => None,
    }
}

/// 逐个覆盖恢复快照；首次写入失败即停止，报告不会声称全部持久化成功。
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

/// 对每个恢复任务的死信执行完整幂等门禁，并仅在总线成功返回 receipt 后记录 replay。
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

            // 原事件已确认意味着业务处理完成，重驱会产生重复副作用，必须跳过。
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
            // 最新 replay 是跨崩溃幂等键：无论已确认、在途还是失败，都不在恢复阶段重复发布。
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

            // 总线先持久化 replay；只有取得 receipt 后才把关联证据追加到任务快照。
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

/// 执行 `skip_redrive` 对应的处理逻辑。
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

/// 执行 `recovery_audit_event` 对应的恢复或重驱流程。
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

/// 将每个仍活动的提供者快照标记为重启中断，并尝试把中断关联回对应任务。
///
/// 非活动进程保持不变；每个活动进程在报告成功前必须完成进程表 upsert，避免仍被误判为
/// 运行中。任务退避计划只描述调度决定，本模块不会直接重启提供者。
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

/// 将提供者会话关联到同请求标识的恢复任务，并按重启策略选择退避或普通中断。
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

/// 同时要求允许的重启策略、显式退避时长和剩余重试次数，才生成调度退避任务。
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

/// 执行 `provider_restart_policy_allows_backoff` 对应的处理逻辑。
fn provider_restart_policy_allows_backoff(policy: &str) -> bool {
    matches!(policy, "scheduler_backoff" | "retry_backoff" | "retryable")
}

/// 声明 `tests` 子模块。
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

    /// 验证 `recovery_marks_incomplete_tasks_without_touching_terminal_tasks` 场景下的预期行为。
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

    /// 验证 `recovery_marks_cancelling_tasks_as_interrupted` 场景下的预期行为。
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

    /// 验证 `recovery_marks_dead_letter_tasks_as_recovering` 场景下的预期行为。
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

    /// 验证 `recovery_persists_task_store_updates` 场景下的预期行为。
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

    /// 验证 `recovery_audit_covers_clean_start_smoke` 场景下的预期行为。
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

    /// 验证 `recovery_redrives_unacked_dead_letters_and_records_task_replay` 场景下的预期行为。
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

    /// 验证 `recovery_audit_covers_restart_redrive_smoke` 场景下的预期行为。
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

    /// 验证 `recovery_skips_redrive_for_acked_original_events` 场景下的预期行为。
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

    /// 验证 `recovery_skips_redrive_until_policy_is_due` 场景下的预期行为。
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

    /// 验证 `recovery_skips_dead_letter_after_scheduler_retry_dispatched_replay` 场景下的预期行为。
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

    /// 验证 `recovery_reports_corrupt_task_store_smoke` 场景下的预期行为。
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

    /// 验证 `recovery_interrupts_active_provider_process_and_preserves_task` 场景下的预期行为。
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

    /// 验证 `recovery_schedules_provider_backoff_only_when_restart_policy_allows_it` 场景下的预期行为。
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

    /// 返回 `snapshot` 对应的数据视图。
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

    /// 执行 `dead_letter` 对应的处理逻辑。
    fn dead_letter(event_id: &str) -> TaskStateDeadLetterSnapshot {
        TaskStateDeadLetterSnapshot {
            event_id: event_id.to_owned(),
            topic: "/input/user".to_owned(),
            reason_kind: "timeout".to_owned(),
            reason: "handler timeout".to_owned(),
            replay_count: 0,
        }
    }

    /// 执行 `event` 对应的处理逻辑。
    fn event(id: &str) -> Event {
        Event::new(
            EventId::parse(id).unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::text("hello"),
        )
    }

    /// 执行 `provider_process` 对应的处理逻辑。
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

    /// 表示 `TestRoot` 数据结构。
    struct TestRoot {
        /// 记录 `path` 字段对应的值。
        path: PathBuf,
    }

    /// 为相关类型实现其约定的行为与方法。
    impl TestRoot {
        /// 返回 `path` 对应的数据视图。
        fn path(&self) -> &Path {
            &self.path
        }
    }

    /// 为相关类型实现其约定的行为与方法。
    impl Drop for TestRoot {
        /// 停止、取消或释放 `drop` 管理的状态。
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// 执行 `test_root` 对应的处理逻辑。
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
