//! 根据持久化任务、事件日志和提供者进程快照恢复守护进程重启后的可判定状态。
//!
//! 恢复不会假定崩溃前的内存执行仍可继续：非终态任务被标记为中断，带死信的任务进入可
//! 重驱态，活动提供者进程统一标记为重启中断。死信重驱在发布前检查原事件确认状态、到期
//! 时间和最新 replay 记录，以事件日志作为跨重启幂等边界。
//! V1.6.4 runtime crash recovery coordinator.

use crate::{TaskHandlerRegistry, TaskKind};
use eva_adapter::{
    decide_restart, restart_due_at_ms, OsProcessBackend, ProcessTerminationOutcome,
    ProcessTerminationReport, RestartDecision, RestartOutcome, DEFAULT_STABLE_RUN_WINDOW_MS,
};
use eva_config::{ProviderRestartConfig, ProviderRestartMode};
use eva_core::{EvaError, EventId};
use eva_eventbus::DurableEventBus;
use eva_observability::{AuditAction, AuditEvent, AuditOutcome, AuditSink, TraceFields};
use eva_scheduler::{decide_retry_backoff, RetryBackoffPolicy};
use eva_storage::{
    EffectLedgerState, EffectOperationIdentity, EventLogStatus, FileSystemEffectLedger,
    FileSystemTaskStateStore, ProviderProcessSnapshot, ProviderProcessTable,
    TaskStateReplaySnapshot, TaskStateSnapshot, TaskStateStore, WriterGeneration,
};
use std::collections::BTreeMap;
use std::time::Duration;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "runtime restart recovery over durable task evidence";

/// Grace period used while reclaiming a provider left behind by a crashed
/// adapter/daemon. The process backend force-kills the complete group or Job
/// after this window and reports the decision in the recovery audit chain.
pub const DEFAULT_PROVIDER_GRACEFUL_TERMINATION_TIMEOUT_MS: u64 = 250;

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
            if status == "queued" && snapshot.replay_delivery.is_some() {
                if snapshot.mark_abandoned_replay_delivery_queued().is_err() {
                    unchanged_tasks.push(snapshot.task_id);
                    continue;
                }
            } else {
                snapshot.status = status.to_owned();
            }
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
        let orphan_scan = cleanup_provider_orphans(process_table)?;
        let mut report = self.recover_snapshots(store.list_snapshots()?);
        append_provider_orphan_audit(&mut report, orphan_scan);
        recover_provider_processes(&mut report, process_table, Some(store))?;
        persist_recovered_snapshots(store, &mut report)?;
        Ok(report)
    }

    /// Recovers tasks from handler and effect-ledger facts before reconciling provider sessions.
    ///
    /// A registered safe handler may reclaim an abandoned attempt. Non-idempotent handlers may do
    /// so only while their operation is absent from the ledger; prepared operations become stable
    /// operator blocks and committed operations repair the task result without invoking a handler.
    pub fn recover_task_store_with_effects_and_provider_processes(
        &self,
        store: &mut FileSystemTaskStateStore,
        handlers: &TaskHandlerRegistry,
        effect_ledger: &mut FileSystemEffectLedger,
        process_table: &mut impl ProviderProcessTable,
    ) -> Result<RuntimeRecoveryReport, EvaError> {
        let orphan_scan = cleanup_provider_orphans(process_table)?;
        let mut report = recover_effect_aware_tasks(store, handlers, effect_ledger)?;
        append_provider_orphan_audit(&mut report, orphan_scan);
        recover_provider_processes(&mut report, process_table, Some(store))?;
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

fn recover_effect_aware_tasks(
    store: &mut FileSystemTaskStateStore,
    handlers: &TaskHandlerRegistry,
    effect_ledger: &mut FileSystemEffectLedger,
) -> Result<RuntimeRecoveryReport, EvaError> {
    let snapshots = store.list_snapshots()?;
    let mut report = empty_recovery_report(snapshots.len());

    for snapshot in snapshots {
        let previous = snapshot.clone();
        let (recovered, decision) =
            recover_effect_aware_task(store, handlers, effect_ledger, snapshot)?;
        report.audit.push(format!(
            "runtime.recovery:task:{}:{decision}",
            previous.task_id
        ));
        if recovered == previous {
            report.unchanged_tasks.push(previous.task_id);
            continue;
        }
        report.recovered_tasks.push(RecoveredTask {
            task_id: recovered.task_id.clone(),
            previous_status: previous.status,
            status: recovered.status.clone(),
            redrive_candidate: recovered.status == "recovering",
        });
        report.recovered_snapshots.push(recovered);
    }

    report
        .audit
        .push(format!("runtime.recovery:scanned:{}", report.scanned_tasks));
    report.audit.push(format!(
        "runtime.recovery:recovered:{}",
        report.recovered_tasks.len()
    ));
    report.audit.push(format!(
        "runtime.recovery:unchanged:{}",
        report.unchanged_tasks.len()
    ));
    report
        .audit
        .push("runtime.recovery:effect_aware:true".to_owned());
    Ok(report)
}

fn recover_effect_aware_task(
    store: &mut FileSystemTaskStateStore,
    handlers: &TaskHandlerRegistry,
    effect_ledger: &mut FileSystemEffectLedger,
    snapshot: TaskStateSnapshot,
) -> Result<(TaskStateSnapshot, &'static str), EvaError> {
    reject_current_generation_live_recovery(store, &snapshot)?;
    let Some(envelope) = snapshot.envelope.as_ref() else {
        return Ok((
            recover_baseline_snapshot(store, &snapshot)?,
            "legacy_conservative",
        ));
    };
    let kind = TaskKind::parse(&envelope.kind)
        .map_err(|error| error.with_context("task_id", &snapshot.task_id))?;
    if !handlers.contains(&kind) {
        return Ok((
            interrupt_unknown_handler_after_restart(store, &snapshot)?,
            "unknown_handler_interrupted",
        ));
    }

    let Some(effect_scope) = handlers.non_idempotent_effect_scope(&kind) else {
        return Ok((
            recover_registered_safe_task(store, &snapshot)?,
            "safe_handler",
        ));
    };
    let operation = EffectOperationIdentity::new(
        &envelope.idempotency_key,
        &envelope.kind,
        &envelope.agent_id,
        effect_scope,
        envelope.input.digest(),
    )
    .map_err(|error| error.with_context("task_id", &snapshot.task_id))?;
    let Some(record) = effect_ledger
        .inspect(&operation)
        .map_err(|error| error.with_context("task_id", &snapshot.task_id))?
    else {
        return Ok((
            recover_registered_safe_task(store, &snapshot)?,
            "effect_absent_safe_retry",
        ));
    };

    match record.state() {
        EffectLedgerState::Prepared => Ok((
            store.block_task_for_unknown_effect(
                &snapshot.task_id,
                record.operation().operation_digest(),
            )?,
            "effect_prepared_operator_block",
        )),
        EffectLedgerState::Committed => {
            let result_digest = record.result_digest().ok_or_else(|| {
                EvaError::conflict("committed effect record is missing its result digest")
                    .with_context("task_id", &snapshot.task_id)
                    .with_context("operation_digest", record.operation().operation_digest())
            })?;
            let result_size_bytes = record.result_size_bytes().ok_or_else(|| {
                EvaError::conflict("committed effect record is missing its result size")
                    .with_context("task_id", &snapshot.task_id)
                    .with_context("operation_digest", record.operation().operation_digest())
            })?;
            Ok((
                store.recover_task_from_committed_effect(
                    &snapshot.task_id,
                    record.operation().operation_digest(),
                    result_digest,
                    result_size_bytes,
                )?,
                "effect_committed_completed",
            ))
        }
    }
}

fn reject_current_generation_live_recovery(
    store: &FileSystemTaskStateStore,
    snapshot: &TaskStateSnapshot,
) -> Result<(), EvaError> {
    if store.runtime_writer_generation() == Some(snapshot.owner_generation)
        && matches!(snapshot.status.as_str(), "running" | "cancelling")
    {
        return Err(EvaError::conflict(
            "restart recovery cannot interrupt a live task from the current writer generation",
        )
        .with_context("task_id", &snapshot.task_id)
        .with_context("status", &snapshot.status)
        .with_context("writer_generation", snapshot.owner_generation.0.to_string()));
    }
    Ok(())
}

fn recover_registered_safe_task(
    store: &mut FileSystemTaskStateStore,
    snapshot: &TaskStateSnapshot,
) -> Result<TaskStateSnapshot, EvaError> {
    if matches!(
        snapshot.status.as_str(),
        "running" | "interrupted" | "recovering"
    ) {
        let recovered = if snapshot.replay_delivery.is_some() {
            store.recover_abandoned_replay_delivery(&snapshot.task_id)?
        } else {
            store.recover_abandoned_task_for_retry(&snapshot.task_id)?
        };
        if recovered != *snapshot || snapshot.status == "interrupted" {
            return Ok(recovered);
        }
        if snapshot.replay_delivery.is_some() && snapshot.dead_letters.is_empty() {
            return interrupt_task_after_restart(
                store,
                snapshot,
                "abandoned replay delivery is not eligible for another attempt",
            );
        }
    }
    recover_baseline_snapshot(store, snapshot)
}

fn interrupt_unknown_handler_after_restart(
    store: &mut FileSystemTaskStateStore,
    snapshot: &TaskStateSnapshot,
) -> Result<TaskStateSnapshot, EvaError> {
    if matches!(
        snapshot.status.as_str(),
        "running" | "cancelling" | "recovering"
    ) {
        return interrupt_task_after_restart(
            store,
            snapshot,
            "registered task handler is unavailable during restart recovery",
        );
    }
    Ok(snapshot.clone())
}

fn interrupt_task_after_restart(
    store: &mut FileSystemTaskStateStore,
    snapshot: &TaskStateSnapshot,
    reason: &str,
) -> Result<TaskStateSnapshot, EvaError> {
    let mut candidate = snapshot.clone();
    candidate.mark_interrupted(reason);
    store.compare_and_set(&candidate)
}

fn recover_baseline_snapshot(
    store: &mut FileSystemTaskStateStore,
    snapshot: &TaskStateSnapshot,
) -> Result<TaskStateSnapshot, EvaError> {
    let Some(status) = recovery_status(snapshot) else {
        return Ok(snapshot.clone());
    };
    if status == "queued" && snapshot.replay_delivery.is_some() {
        return store.recover_abandoned_replay_delivery(&snapshot.task_id);
    }
    let mut candidate = snapshot.clone();
    let previous_status = candidate.status.clone();
    candidate.status = status.to_owned();
    candidate.push_log(
        "warning",
        format!("runtime recovery marked {previous_status} task as {status} after restart"),
    );
    store.compare_and_set(&candidate)
}

fn persist_provider_task_recovery(
    store: &mut FileSystemTaskStateStore,
    report: &mut RuntimeRecoveryReport,
    task_id: &str,
) -> Result<bool, EvaError> {
    let Some(snapshot) = report
        .recovered_snapshots
        .iter_mut()
        .find(|snapshot| snapshot.task_id == task_id)
    else {
        return Ok(false);
    };
    let current = store.read(Some(task_id))?;
    if current == *snapshot {
        return Ok(false);
    }
    if current.record_version != snapshot.record_version {
        return Err(
            EvaError::conflict("provider task recovery lost its task-state CAS authority")
                .with_context("task_id", task_id)
                .with_context("expected", snapshot.record_version.0.to_string())
                .with_context("actual", current.record_version.0.to_string()),
        );
    }
    *snapshot = store.compare_and_set(snapshot)?;
    Ok(true)
}

fn empty_recovery_report(scanned_tasks: usize) -> RuntimeRecoveryReport {
    RuntimeRecoveryReport {
        scanned_tasks,
        recovered_tasks: Vec::new(),
        unchanged_tasks: Vec::new(),
        recovered_snapshots: Vec::new(),
        redriven_events: Vec::new(),
        skipped_redrive_events: Vec::new(),
        scanned_provider_processes: 0,
        recovered_provider_processes: Vec::new(),
        unchanged_provider_processes: Vec::new(),
        provider_backoff_tasks: Vec::new(),
        skipped_provider_tasks: Vec::new(),
        audit: Vec::new(),
    }
}

/// 只转换崩溃时可能仍在推进的状态；完成、失败等终态返回 `None` 并保持原样。
fn recovery_status(snapshot: &TaskStateSnapshot) -> Option<&'static str> {
    if !snapshot.dead_letters.is_empty()
        && matches!(
            snapshot.status.as_str(),
            "queued" | "running" | "cancelling"
        )
    {
        return Some("recovering");
    }
    if snapshot.replay_delivery.is_some() && !snapshot.cancel_requested && !snapshot.cancel_accepted
    {
        return match snapshot.status.as_str() {
            "queued" => None,
            "running" | "interrupted" | "recovering" => Some("queued"),
            _ => None,
        };
    }
    match snapshot.status.as_str() {
        "queued" => None,
        "running" | "cancelling" => Some("interrupted"),
        _ => None,
    }
}

/// 逐个覆盖恢复快照；首次写入失败即停止，报告不会声称全部持久化成功。
fn persist_recovered_snapshots(
    store: &mut FileSystemTaskStateStore,
    report: &mut RuntimeRecoveryReport,
) -> Result<(), EvaError> {
    let mut persisted = 0usize;
    for snapshot in &mut report.recovered_snapshots {
        if store.read(Some(&snapshot.task_id))? == *snapshot {
            continue;
        }
        *snapshot = if snapshot.replay_delivery.is_some() && snapshot.status == "queued" {
            store.recover_abandoned_replay_delivery(&snapshot.task_id)?
        } else {
            store.compare_and_set(snapshot)?
        };
        persisted += 1;
    }
    report
        .audit
        .push(format!("runtime.recovery:persisted:{persisted}"));
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
#[derive(Debug)]
struct ProviderOrphanScan {
    scanned: usize,
    outcomes: BTreeMap<String, ProcessTerminationReport>,
}

/// Fence and clean every active provider boundary before task-state recovery
/// is allowed to mutate or requeue work. This ordering prevents an abandoned
/// provider from racing a newly recovered attempt.
fn cleanup_provider_orphans(
    process_table: &impl ProviderProcessTable,
) -> Result<ProviderOrphanScan, EvaError> {
    let processes = process_table.list()?;
    let current_generation = process_table.writer_generation();
    let mut outcomes = BTreeMap::new();

    for process in processes.iter().filter(|process| process.active) {
        if current_generation.is_some_and(|generation| {
            generation != WriterGeneration::ZERO && process.owner_generation == generation
        }) {
            return Err(EvaError::conflict(
                "recovery cannot reclaim a provider process owned by the current writer",
            )
            .with_context("session_id", &process.session_id)
            .with_context("owner_generation", process.owner_generation.0.to_string()));
        }

        let termination = OsProcessBackend::new()
            .terminate_snapshot(
                process,
                Duration::from_millis(DEFAULT_PROVIDER_GRACEFUL_TERMINATION_TIMEOUT_MS),
            )
            .map_err(|error| {
                error
                    .with_context("session_id", &process.session_id)
                    .with_context("provider_process_id", &process.provider_process_id)
            })?;
        if matches!(
            termination.outcome,
            ProcessTerminationOutcome::IdentityMismatch
                | ProcessTerminationOutcome::MissingIdentity
        ) {
            return Err(EvaError::conflict(
                "provider orphan identity cannot prove the recorded boundary is safe to reclaim",
            )
            .with_context("session_id", &process.session_id)
            .with_context("pid", process.pid.unwrap_or_default().to_string())
            .with_context("cleanup_outcome", termination.outcome.as_str())
            .with_context("cleanup_boundary", &termination.boundary));
        }
        outcomes.insert(process.session_id.clone(), termination);
    }

    Ok(ProviderOrphanScan {
        scanned: processes.len(),
        outcomes,
    })
}

fn append_provider_orphan_audit(report: &mut RuntimeRecoveryReport, scan: ProviderOrphanScan) {
    let active = scan.outcomes.len();
    let cleaned = scan
        .outcomes
        .values()
        .filter(|termination| {
            matches!(
                termination.outcome,
                ProcessTerminationOutcome::AlreadyExited
                    | ProcessTerminationOutcome::Graceful
                    | ProcessTerminationOutcome::Forced
            )
        })
        .count();
    for (session_id, termination) in scan.outcomes {
        report.audit.extend(
            termination
                .audit_entries()
                .into_iter()
                .map(|entry| format!("runtime.recovery:{entry}")),
        );
        report.audit.push(format!(
            "runtime.recovery:provider_orphan:{session_id}:{}",
            termination.outcome.as_str()
        ));
    }
    report.audit.push(format!(
        "runtime.recovery:provider_orphan_scanned:{}",
        scan.scanned
    ));
    report
        .audit
        .push(format!("runtime.recovery:provider_orphan_active:{active}"));
    report.audit.push(format!(
        "runtime.recovery:provider_orphan_cleaned:{cleaned}"
    ));
}

fn recover_provider_processes(
    report: &mut RuntimeRecoveryReport,
    process_table: &mut impl ProviderProcessTable,
    mut task_store: Option<&mut FileSystemTaskStateStore>,
) -> Result<(), EvaError> {
    let processes = process_table.list()?;
    report.scanned_provider_processes = processes.len();

    for mut process in processes {
        if !process.active {
            if matches!(process.restart_state.as_str(), "starting" | "pending") {
                let previous_health = process.health.clone();
                if process.restart_state == "starting" {
                    process.recover_starting_restart(epoch_millis_now())?;
                }
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
                if let Some(store) = task_store.as_deref_mut() {
                    let persisted = persist_provider_task_recovery(store, report, &task_id)?;
                    if persisted {
                        report.audit.push(format!(
                            "runtime.recovery:provider_task_persisted:{task_id}"
                        ));
                    }
                }
                let committed = process_table.compare_and_set(process)?;
                report
                    .recovered_provider_processes
                    .push(RecoveredProviderProcess {
                        session_id: committed.session_id,
                        provider_process_id: committed.provider_process_id,
                        request_id: committed.request_id.as_str().to_owned(),
                        adapter_id: committed.adapter_id.as_str().to_owned(),
                        previous_health,
                        health: committed.health,
                        task_id,
                        task_status,
                        retry_scheduled,
                    });
                continue;
            }
            report.unchanged_provider_processes.push(process.session_id);
            continue;
        }

        let previous_health = process.health.clone();
        let task_id = process.request_id.as_str().to_owned();
        reset_restart_budget_after_stable_run(&mut process, epoch_millis_now())?;
        let restart_decision = recovery_restart_decision(&process);
        match restart_decision {
            RestartDecision::Restart { attempt, delay_ms } => {
                process.mark_restart_pending(
                    attempt,
                    restart_due_at_ms(epoch_millis_now(), delay_ms),
                    "daemon restart recovered provider crash",
                )?;
            }
            RestartDecision::BudgetExhausted => {
                process.mark_restart_exhausted("daemon restart found exhausted provider budget")?;
            }
            RestartDecision::NoRestart => {
                process.mark_interrupted_after_restart(
                    "daemon restart interrupted active provider session",
                );
            }
        }
        let task_status = recover_provider_task(report, &process);
        let retry_scheduled = task_status
            .as_ref()
            .map(|status| status == "recovering")
            .unwrap_or(false)
            && report
                .provider_backoff_tasks
                .iter()
                .any(|entry| entry.session_id == process.session_id);
        if let Some(store) = task_store.as_deref_mut() {
            let persisted = persist_provider_task_recovery(store, report, &task_id)?;
            if persisted {
                report.audit.push(format!(
                    "runtime.recovery:provider_task_persisted:{task_id}"
                ));
            }
        }
        let committed = process_table.compare_and_set(process)?;

        report
            .recovered_provider_processes
            .push(RecoveredProviderProcess {
                session_id: committed.session_id,
                provider_process_id: committed.provider_process_id,
                request_id: committed.request_id.as_str().to_owned(),
                adapter_id: committed.adapter_id.as_str().to_owned(),
                previous_health,
                health: committed.health,
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
    let effect_aware = report
        .audit
        .iter()
        .any(|entry| entry == "runtime.recovery:effect_aware:true");
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

    let protected_reason = if !effect_aware {
        None
    } else if snapshot.status == "completed" {
        Some("task_completed_by_effect_recovery")
    } else if snapshot.status == "queued" {
        Some("task_requeued_by_restart_recovery")
    } else if snapshot.requires_operator_reconciliation() {
        Some("effect_outcome_requires_operator_reconciliation")
    } else if snapshot.status == "interrupted" {
        Some("task_already_interrupted_by_restart_recovery")
    } else {
        None
    };
    if let Some(reason) = protected_reason {
        report.skipped_provider_tasks.push(SkippedProviderTask {
            task_id: task_id.to_owned(),
            session_id: process.session_id.clone(),
            reason: reason.to_owned(),
        });
        return Some(snapshot.status.clone());
    }

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
    if process.restart_state == "pending" {
        let due_at_ms = process.restart_due_at_ms?;
        let now = epoch_millis_now();
        let due_after_ms = due_at_ms.saturating_sub(now).try_into().unwrap_or(u64::MAX);
        return Some(ProviderBackoffTask {
            task_id: snapshot.task_id.clone(),
            session_id: process.session_id.clone(),
            next_attempt: process.restart_attempts,
            due_after_ms,
            reason: "provider_restart_backoff".to_owned(),
        });
    }
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
    matches!(
        policy,
        "scheduler_backoff" | "retry_backoff" | "retryable" | "on_failure" | "always"
    )
}

fn recovery_restart_decision(process: &ProviderProcessSnapshot) -> RestartDecision {
    let mode = ProviderRestartMode::parse(&process.restart_policy).unwrap_or_default();
    decide_restart(
        ProviderRestartConfig {
            mode,
            max_attempts: process.restart_max_attempts,
            backoff_ms: process.restart_backoff_ms,
        },
        process.restart_attempts,
        RestartOutcome::Failure,
        &process.session_id,
    )
}

fn reset_restart_budget_after_stable_run(
    process: &mut ProviderProcessSnapshot,
    now_ms: u128,
) -> Result<(), EvaError> {
    if process.restart_attempts == 0 || process.restart_state != "running" {
        return Ok(());
    }
    let elapsed_ms = now_ms.saturating_sub(process.updated_at_ms);
    if elapsed_ms < u128::from(DEFAULT_STABLE_RUN_WINDOW_MS) {
        return Ok(());
    }

    process.mark_stable_success()?;
    process.audit.push(format!(
        "provider.restart:stable_window_elapsed_ms:{elapsed_ms}"
    ));
    Ok(())
}

fn epoch_millis_now() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TaskHandlerInvocation, TaskHandlerResult, TaskWorkerRuntime};
    use eva_core::{AgentId, EvaError, Event, EventId, EventPayload, Topic};
    use eva_eventbus::{DurableEventBus, EventBus, RedrivePolicy};
    use eva_observability::{SpanId, TraceFields};
    use eva_storage::{
        DurableBackendOptions, EffectLedgerIntent, FileSystemAuditSink, FileSystemDurableBackend,
        FileSystemProviderProcessTable, InMemoryArtifactStore, InMemoryProviderProcessTable,
        ProviderProcessTable, StateVersion, TaskAttemptOutcome, TaskAttemptPolicySnapshot,
        TaskEnvelopeSnapshot, TaskStateDeadLetterSnapshot, TaskStateLogSnapshot, TaskStateStore,
        WriterGeneration,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

    #[test]
    fn recovery_leaves_ordinary_queued_tasks_ready_for_worker_claim() {
        let queued = snapshot("req-recovery-queued", "queued");
        let report = RuntimeRecoveryCoordinator.recover_snapshots(vec![queued]);

        assert!(report.recovered_tasks.is_empty());
        assert!(report.recovered_snapshots.is_empty());
        assert_eq!(report.unchanged_tasks, vec!["req-recovery-queued"]);
    }

    #[test]
    fn recovery_requeues_abandoned_replay_delivery_across_writer_generation() {
        let root = test_root("owned-replay-generation");
        let task_id = format!("replay-delivery-{}", "a".repeat(64));
        let envelope = TaskEnvelopeSnapshot::inline(
            "runtime.echo",
            "root-agent",
            b"replay-payload".to_vec(),
            task_id.clone(),
            TaskAttemptPolicySnapshot::new(u32::MAX, 0, None).unwrap(),
        )
        .unwrap();
        let (first_generation, running_snapshot) = {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            store
                .create(
                    &TaskStateSnapshot::queued_with_replay_delivery(
                        &task_id,
                        envelope.clone(),
                        "evt-owned-replay:replay-1",
                        0,
                    )
                    .unwrap(),
                )
                .unwrap();
            let running = store
                .try_claim_queued(&task_id, "daemon.old.worker", "cancel.old.replay", 100)
                .unwrap()
                .unwrap()
                .snapshot()
                .clone();
            (running.owner_generation, running)
        };

        let classified = RuntimeRecoveryCoordinator.recover_snapshots(vec![running_snapshot]);
        assert_eq!(classified.recovered_snapshots[0].status, "queued");
        classified.recovered_snapshots[0].validate().unwrap();

        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let report = RuntimeRecoveryCoordinator
            .recover_task_store(&mut store)
            .unwrap();
        assert_eq!(report.recovered_tasks.len(), 1);
        assert_eq!(report.recovered_tasks[0].previous_status, "running");
        assert_eq!(report.recovered_tasks[0].status, "queued");
        let recovered = store.read(Some(&task_id)).unwrap();
        let second_generation = recovered.owner_generation;
        assert_ne!(first_generation, second_generation);
        assert_eq!(recovered.status, "queued");
        assert_eq!(recovered.owner_generation, second_generation);
        assert_eq!(recovered.attempts, 1);
        assert!(recovered.execution_owner.is_none());
        assert!(recovered.cancel_token.is_none());
        assert_eq!(recovered.envelope.as_ref(), Some(&envelope));
        assert!(recovered.replay_delivery.is_some());

        let resumed = store
            .try_claim_queued(&task_id, "daemon.new.worker", "cancel.new.replay", 200)
            .unwrap()
            .unwrap();
        assert_eq!(resumed.snapshot().attempts, 2);
        assert_eq!(resumed.fence().owner_generation(), second_generation);
    }

    #[test]
    fn effect_aware_recovery_resolves_all_crash_boundaries_before_worker_start() {
        let root = test_root("effect-aware-crash-boundaries");
        let pure_task_id = "req-recovery-pure-abandoned";
        let absent_task_id = "req-recovery-effect-absent";
        let prepared_task_id = "req-recovery-effect-prepared";
        let committed_task_id = "req-recovery-effect-committed";
        let pure_envelope =
            recovery_envelope("runtime.echo", "idem-recovery-pure-abandoned", b"pure", 2);
        let absent_envelope = recovery_envelope(
            "runtime.effect.absent",
            "idem-recovery-effect-absent",
            b"absent",
            2,
        );
        let prepared_envelope = recovery_envelope(
            "runtime.effect.prepared",
            "idem-recovery-effect-prepared",
            b"prepared",
            2,
        );
        let committed_envelope = recovery_envelope(
            "runtime.effect.committed",
            "idem-recovery-effect-committed",
            b"committed",
            2,
        );
        let committed_result = TaskHandlerResult::new(b"already-committed".to_vec());

        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let writer = backend.acquire_runtime_writer().unwrap();
            let mut store =
                FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
                    .unwrap();
            let mut ledger =
                FileSystemEffectLedger::open_with_writer(backend.layout(), writer).unwrap();

            create_and_claim_recovery_task(&mut store, pure_task_id, &pure_envelope);
            create_and_claim_recovery_task(&mut store, absent_task_id, &absent_envelope);
            let prepared_claim =
                create_and_claim_recovery_task(&mut store, prepared_task_id, &prepared_envelope);
            let prepared_intent = recovery_effect_intent(
                &prepared_envelope,
                "effect-scope-recovery-prepared",
                &prepared_claim,
            );
            assert!(matches!(
                ledger.prepare_for_claim(&store, &prepared_intent).unwrap(),
                eva_storage::EffectPrepareOutcome::Created(_)
            ));

            let committed_claim =
                create_and_claim_recovery_task(&mut store, committed_task_id, &committed_envelope);
            let committed_intent = recovery_effect_intent(
                &committed_envelope,
                "effect-scope-recovery-committed",
                &committed_claim,
            );
            assert!(matches!(
                ledger.prepare_for_claim(&store, &committed_intent).unwrap(),
                eva_storage::EffectPrepareOutcome::Created(_)
            ));
            ledger
                .commit(
                    &committed_intent,
                    committed_result.digest(),
                    committed_result.size_bytes(),
                    epoch_millis(),
                )
                .unwrap();
            store
                .finish_execution(
                    committed_claim.fence(),
                    &TaskAttemptOutcome::Failed {
                        error_kind: "unavailable".to_owned(),
                        error_message: "task checkpoint contradicted committed effect".to_owned(),
                        retryable: false,
                    },
                )
                .unwrap();

            let mut processes = FileSystemProviderProcessTable::from_runtime_writer(
                backend.layout(),
                store.runtime_writer().unwrap(),
            )
            .unwrap();
            processes
                .upsert(terminated_provider_process(
                    "session-effect-committed",
                    committed_task_id,
                    "scheduler_backoff",
                    Some(25),
                ))
                .unwrap();
        }

        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut store =
            FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
                .unwrap();
        let mut ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let mut processes = FileSystemProviderProcessTable::from_runtime_writer(
            backend.layout(),
            store.runtime_writer().unwrap(),
        )
        .unwrap();
        let effect_calls = Arc::new(AtomicUsize::new(0));
        let registry = Arc::new(recovery_handler_registry(Arc::clone(&effect_calls)));

        let report = RuntimeRecoveryCoordinator
            .recover_task_store_with_effects_and_provider_processes(
                &mut store,
                registry.as_ref(),
                &mut ledger,
                &mut processes,
            )
            .unwrap();
        let pure = store.read(Some(pure_task_id)).unwrap();
        let absent = store.read(Some(absent_task_id)).unwrap();
        let prepared = store.read(Some(prepared_task_id)).unwrap();
        let committed = store.read(Some(committed_task_id)).unwrap();
        assert_eq!(pure.status, "queued");
        assert_eq!(pure.attempts, 1);
        assert_eq!(absent.status, "queued");
        assert_eq!(absent.attempts, 1);
        assert_eq!(prepared.status, "interrupted");
        assert!(prepared.requires_operator_reconciliation());
        assert_eq!(committed.status, "completed");
        assert_eq!(
            committed.result_digest.as_deref(),
            Some(committed_result.digest())
        );
        assert_eq!(
            committed.result_size_bytes,
            Some(committed_result.size_bytes())
        );
        assert_eq!(effect_calls.load(Ordering::SeqCst), 0);
        assert!(report.skipped_provider_tasks.iter().any(|entry| {
            entry.task_id == committed_task_id
                && entry.reason == "task_completed_by_effect_recovery"
        }));
        assert!(!processes.read("session-effect-committed").unwrap().active);

        let snapshots_before_repeat = [
            pure.clone(),
            absent.clone(),
            prepared.clone(),
            committed.clone(),
        ];
        let repeated = RuntimeRecoveryCoordinator
            .recover_task_store_with_effects_and_provider_processes(
                &mut store,
                registry.as_ref(),
                &mut ledger,
                &mut processes,
            )
            .unwrap();
        assert!(repeated.recovered_tasks.is_empty());
        for expected in snapshots_before_repeat {
            assert_eq!(store.read(Some(&expected.task_id)).unwrap(), expected);
        }

        let failure_bus =
            DurableEventBus::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let mut worker = TaskWorkerRuntime::start_paused_with_durable_services(
            store.clone(),
            registry,
            Arc::new(InMemoryArtifactStore::default()),
            "daemon.recovery.worker",
            failure_bus,
            ledger,
        )
        .unwrap();
        worker.activate();
        wait_for_recovery_status(&store, pure_task_id, "completed");
        wait_for_recovery_status(&store, absent_task_id, "completed");
        assert_eq!(effect_calls.load(Ordering::SeqCst), 1);
        assert!(store
            .read(Some(prepared_task_id))
            .unwrap()
            .requires_operator_reconciliation());
        assert_eq!(store.read(Some(committed_task_id)).unwrap(), committed);
        worker.stop_and_join().unwrap();
    }

    #[test]
    fn effect_aware_recovery_rejects_current_generation_running_task() {
        let root = test_root("effect-aware-current-generation");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut store =
            FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
                .unwrap();
        let mut ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer).unwrap();
        let task_id = "req-recovery-current-generation";
        create_and_claim_recovery_task(
            &mut store,
            task_id,
            &recovery_envelope(
                "runtime.echo",
                "idem-recovery-current-generation",
                b"live",
                2,
            ),
        );
        let mut processes = FileSystemProviderProcessTable::from_runtime_writer(
            backend.layout(),
            store.runtime_writer().unwrap(),
        )
        .unwrap();
        let registry = TaskHandlerRegistry::with_runtime_defaults().unwrap();

        let error = RuntimeRecoveryCoordinator
            .recover_task_store_with_effects_and_provider_processes(
                &mut store,
                &registry,
                &mut ledger,
                &mut processes,
            )
            .unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(store.read(Some(task_id)).unwrap().status, "running");

        let unknown_task_id = "req-recovery-current-unknown";
        create_and_claim_recovery_task(
            &mut store,
            unknown_task_id,
            &recovery_envelope(
                "runtime.unknown",
                "idem-recovery-current-unknown",
                b"unknown",
                2,
            ),
        );
        let cancelling_task_id = "req-recovery-current-cancelling";
        create_and_claim_recovery_task(
            &mut store,
            cancelling_task_id,
            &recovery_envelope(
                "runtime.echo",
                "idem-recovery-current-cancelling",
                b"cancelling",
                2,
            ),
        );
        store
            .request_cancellation(cancelling_task_id, "current cancellation")
            .unwrap();
        let legacy_task_id = "req-recovery-current-legacy";
        let mut legacy = TaskStateSnapshot::queued(legacy_task_id).unwrap();
        legacy.mark_running(epoch_millis(), None, "cancel.current.legacy");
        store.create(&legacy).unwrap();

        for task_id in [unknown_task_id, cancelling_task_id, legacy_task_id] {
            let snapshot = store.read(Some(task_id)).unwrap();
            let error =
                recover_effect_aware_task(&mut store, &registry, &mut ledger, snapshot.clone())
                    .unwrap_err();
            assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
            assert_eq!(store.read(Some(task_id)).unwrap(), snapshot);
        }
    }

    #[test]
    fn effect_aware_recovery_fails_closed_on_effect_identity_collision() {
        let root = test_root("effect-aware-identity-collision");
        let source_task_id = "req-z-recovery-effect-source";
        let collision_task_id = "req-a-recovery-effect-collision";
        let source = recovery_envelope(
            "runtime.effect.prepared",
            "idem-recovery-effect-collision",
            b"source",
            2,
        );
        let collision = recovery_envelope(
            "runtime.effect.prepared",
            "idem-recovery-effect-collision",
            b"collision",
            2,
        );
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let writer = backend.acquire_runtime_writer().unwrap();
            let mut store =
                FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
                    .unwrap();
            let mut ledger =
                FileSystemEffectLedger::open_with_writer(backend.layout(), writer).unwrap();
            let source_claim = create_and_claim_recovery_task(&mut store, source_task_id, &source);
            create_and_claim_recovery_task(&mut store, collision_task_id, &collision);
            let source_intent =
                recovery_effect_intent(&source, "effect-scope-recovery-prepared", &source_claim);
            ledger.prepare_for_claim(&store, &source_intent).unwrap();
        }

        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut store =
            FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
                .unwrap();
        let mut ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer).unwrap();
        let registry = recovery_handler_registry(Arc::new(AtomicUsize::new(0)));
        let mut processes = FileSystemProviderProcessTable::from_runtime_writer(
            backend.layout(),
            store.runtime_writer().unwrap(),
        )
        .unwrap();

        let error = RuntimeRecoveryCoordinator
            .recover_task_store_with_effects_and_provider_processes(
                &mut store,
                &registry,
                &mut ledger,
                &mut processes,
            )
            .unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(
            store.read(Some(collision_task_id)).unwrap().status,
            "running"
        );
        assert_eq!(store.read(Some(source_task_id)).unwrap().status, "running");
    }

    #[test]
    fn effect_aware_recovery_never_requeues_exhausted_cancelled_dead_letter_or_unknown_tasks() {
        let root = test_root("effect-aware-requeue-gates");
        let exhausted_task_id = "req-recovery-exhausted";
        let cancelled_task_id = "req-recovery-cancelled";
        let dead_letter_task_id = "req-recovery-dead-letter";
        let unknown_task_id = "req-recovery-unknown-handler";
        let queued_task_id = "req-recovery-already-queued";
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let writer = backend.acquire_runtime_writer().unwrap();
            let mut store =
                FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer).unwrap();
            create_and_claim_recovery_task(
                &mut store,
                exhausted_task_id,
                &recovery_envelope("runtime.echo", "idem-recovery-exhausted", b"exhausted", 1),
            );
            store
                .create(
                    &TaskStateSnapshot::queued_with_envelope(
                        cancelled_task_id,
                        recovery_envelope(
                            "runtime.echo",
                            "idem-recovery-cancelled",
                            b"cancelled",
                            2,
                        ),
                    )
                    .unwrap(),
                )
                .unwrap();
            store
                .request_cancellation(cancelled_task_id, "operator cancelled before restart")
                .unwrap();
            create_and_claim_recovery_task(
                &mut store,
                dead_letter_task_id,
                &recovery_envelope(
                    "runtime.echo",
                    "idem-recovery-dead-letter",
                    b"dead-letter",
                    2,
                ),
            );
            store
                .update_snapshot(dead_letter_task_id, |snapshot| {
                    snapshot.dead_letters.push(dead_letter("evt-recovery-gate"));
                    Ok(())
                })
                .unwrap();
            create_and_claim_recovery_task(
                &mut store,
                unknown_task_id,
                &recovery_envelope(
                    "runtime.unknown",
                    "idem-recovery-unknown-handler",
                    b"unknown",
                    2,
                ),
            );
            store
                .create(
                    &TaskStateSnapshot::queued_with_envelope(
                        queued_task_id,
                        recovery_envelope(
                            "runtime.echo",
                            "idem-recovery-already-queued",
                            b"queued",
                            2,
                        ),
                    )
                    .unwrap(),
                )
                .unwrap();
        }

        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut store =
            FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
                .unwrap();
        let mut ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer).unwrap();
        let registry = TaskHandlerRegistry::with_runtime_defaults().unwrap();
        let mut processes = FileSystemProviderProcessTable::from_runtime_writer(
            backend.layout(),
            store.runtime_writer().unwrap(),
        )
        .unwrap();

        RuntimeRecoveryCoordinator
            .recover_task_store_with_effects_and_provider_processes(
                &mut store,
                &registry,
                &mut ledger,
                &mut processes,
            )
            .unwrap();
        let expected = [
            store.read(Some(exhausted_task_id)).unwrap(),
            store.read(Some(cancelled_task_id)).unwrap(),
            store.read(Some(dead_letter_task_id)).unwrap(),
            store.read(Some(unknown_task_id)).unwrap(),
            store.read(Some(queued_task_id)).unwrap(),
        ];
        assert_eq!(expected[0].status, "interrupted");
        assert_eq!(expected[1].status, "cancelled");
        assert_eq!(expected[2].status, "recovering");
        assert_eq!(expected[3].status, "interrupted");
        assert_eq!(expected[4].status, "queued");

        let repeated = RuntimeRecoveryCoordinator
            .recover_task_store_with_effects_and_provider_processes(
                &mut store,
                &registry,
                &mut ledger,
                &mut processes,
            )
            .unwrap();
        assert!(repeated.recovered_tasks.is_empty());
        for snapshot in expected {
            assert_eq!(store.read(Some(&snapshot.task_id)).unwrap(), snapshot);
        }
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
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
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
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            let mut bus = DurableEventBus::open_with_writer(
                backend.layout(),
                store.runtime_writer().unwrap().clone(),
            )
            .unwrap();
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
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            let mut bus = DurableEventBus::open_with_writer(
                backend.layout(),
                store.runtime_writer().unwrap().clone(),
            )
            .unwrap();

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
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
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
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
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
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
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
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
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
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
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
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
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
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            let mut bus = DurableEventBus::open_with_writer(
                backend.layout(),
                store.runtime_writer().unwrap().clone(),
            )
            .unwrap();
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
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            let mut bus = DurableEventBus::open_with_writer(
                backend.layout(),
                store.runtime_writer().unwrap().clone(),
            )
            .unwrap();

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
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();

        let error = RuntimeRecoveryCoordinator
            .recover_task_store(&mut store)
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
    }

    /// 验证 `recovery_interrupts_active_provider_process_and_preserves_task` 场景下的预期行为。
    #[test]
    fn recovery_interrupts_active_provider_process_and_preserves_task() {
        let root = test_root("provider-interrupted");
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            let mut processes = FileSystemProviderProcessTable::from_runtime_writer(
                backend.layout(),
                store.runtime_writer().unwrap(),
            )
            .unwrap();
            let mut task = snapshot("req-provider-recovery-interrupted", "running");
            task.retry_max_attempts = 3;
            store.write(&task).unwrap();
            processes
                .upsert(terminated_provider_process(
                    "session-provider-recovery-interrupted",
                    "req-provider-recovery-interrupted",
                    "none",
                    None,
                ))
                .unwrap();
        }

        // Recovery must observe a record owned by the previous runtime writer.
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let mut processes = FileSystemProviderProcessTable::from_runtime_writer(
            backend.layout(),
            store.runtime_writer().unwrap(),
        )
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
        assert!(report.audit.iter().any(|entry| {
            entry
                == "runtime.recovery:provider_orphan:session-provider-recovery-interrupted:already_exited"
        }));
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "runtime.recovery:provider.cleanup:forced:false"));
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
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            let mut processes = FileSystemProviderProcessTable::from_runtime_writer(
                backend.layout(),
                store.runtime_writer().unwrap(),
            )
            .unwrap();
            let mut task = snapshot("req-provider-recovery-backoff", "running");
            task.attempts = 1;
            task.retry_max_attempts = 3;
            store.write(&task).unwrap();
            processes
                .upsert(terminated_provider_process(
                    "session-provider-recovery-backoff",
                    "req-provider-recovery-backoff",
                    "scheduler_backoff",
                    Some(2500),
                ))
                .unwrap();
        }

        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let mut processes = FileSystemProviderProcessTable::from_runtime_writer(
            backend.layout(),
            store.runtime_writer().unwrap(),
        )
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

    #[test]
    fn recovery_preserves_auto_restart_attempt_after_orphan_cleanup_and_generation_change() {
        let root = test_root("provider-auto-restart-generation");
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            let mut processes = FileSystemProviderProcessTable::from_runtime_writer(
                backend.layout(),
                store.runtime_writer().unwrap(),
            )
            .unwrap();
            let mut task = snapshot("req-provider-auto-restart-generation", "running");
            task.retry_max_attempts = 3;
            store.write(&task).unwrap();

            let mut process = terminated_provider_process(
                "session-provider-auto-restart-generation",
                "req-provider-auto-restart-generation",
                "on_failure",
                None,
            );
            process.configure_restart_budget(3, 1).unwrap();
            process.restart_attempts = 1;
            process.restart_state = "running".to_owned();
            processes.upsert(process).unwrap();
        }

        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let mut processes = FileSystemProviderProcessTable::from_runtime_writer(
            backend.layout(),
            store.runtime_writer().unwrap(),
        )
        .unwrap();
        let report = RuntimeRecoveryCoordinator
            .recover_task_store_with_provider_processes(&mut store, &mut processes)
            .unwrap();
        let recovered_process = processes
            .read("session-provider-auto-restart-generation")
            .unwrap();
        let recovered_task = store
            .read(Some("req-provider-auto-restart-generation"))
            .unwrap();

        assert_eq!(recovered_process.restart_state, "pending");
        assert_eq!(recovered_process.restart_attempts, 2);
        assert!(recovered_process.restart_due_at_ms.is_some());
        assert_eq!(recovered_task.status, "recovering");
        assert_eq!(report.provider_backoff_tasks.len(), 1);
        assert_eq!(report.provider_backoff_tasks[0].next_attempt, 2);
        assert!(report.audit.iter().any(|entry| {
            entry
                == "runtime.recovery:provider_orphan:session-provider-auto-restart-generation:already_exited"
        }));
    }

    #[test]
    fn recovery_resets_auto_restart_budget_only_after_stable_window() {
        let root = test_root("provider-auto-restart-stable-window");
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
            let mut processes = FileSystemProviderProcessTable::from_runtime_writer(
                backend.layout(),
                store.runtime_writer().unwrap(),
            )
            .unwrap();
            let task_id = "req-provider-auto-restart-stable-window";
            store.write(&snapshot(task_id, "running")).unwrap();

            let mut process = terminated_provider_process(
                "session-provider-auto-restart-stable-window",
                task_id,
                "on_failure",
                None,
            );
            process.configure_restart_budget(3, 1).unwrap();
            process.restart_attempts = 1;
            process.restart_state = "running".to_owned();
            process.updated_at_ms = epoch_millis()
                .saturating_sub(u128::from(eva_adapter::DEFAULT_STABLE_RUN_WINDOW_MS))
                .saturating_sub(1);
            processes.upsert(process).unwrap();
        }

        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let mut store = FileSystemTaskStateStore::from_runtime_writer(
            backend.layout(),
            backend.acquire_runtime_writer().unwrap(),
        )
        .unwrap();
        let mut processes = FileSystemProviderProcessTable::from_runtime_writer(
            backend.layout(),
            store.runtime_writer().unwrap(),
        )
        .unwrap();
        let report = RuntimeRecoveryCoordinator
            .recover_task_store_with_provider_processes(&mut store, &mut processes)
            .unwrap();
        let recovered_process = processes
            .read("session-provider-auto-restart-stable-window")
            .unwrap();

        assert_eq!(recovered_process.restart_state, "pending");
        assert_eq!(recovered_process.restart_attempts, 1);
        assert!(recovered_process
            .audit
            .iter()
            .any(|entry| entry == "provider.restart:stable_reset"));
        assert!(recovered_process
            .audit
            .iter()
            .any(|entry| { entry.starts_with("provider.restart:stable_window_elapsed_ms:") }));
        assert_eq!(report.provider_backoff_tasks[0].next_attempt, 1);
    }

    #[test]
    fn recovery_refuses_pid_reuse_without_mutating_task_or_process_record() {
        let root = test_root("provider-identity-mismatch");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let mut effect_ledger = FileSystemEffectLedger::open_with_writer(
            backend.layout(),
            store.runtime_writer().unwrap(),
        )
        .unwrap();
        let handlers = TaskHandlerRegistry::with_runtime_defaults().unwrap();
        let task_id = "req-provider-recovery-identity-mismatch";
        store.write(&snapshot(task_id, "running")).unwrap();

        let mut command = provider_sleep_command();
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let process_backend = OsProcessBackend::new();
        let mut handle = process_backend.spawn(command).unwrap();
        let mut process = provider_process(
            "session-provider-recovery-identity-mismatch",
            task_id,
            "none",
            None,
        );
        handle.identity().stamp_snapshot(&mut process, 1).unwrap();
        process.process_start_token = Some("reused-process-incarnation".to_owned());
        let mut processes = InMemoryProviderProcessTable::new();
        processes.upsert(process).unwrap();

        let error = RuntimeRecoveryCoordinator
            .recover_task_store_with_effects_and_provider_processes(
                &mut store,
                &handlers,
                &mut effect_ledger,
                &mut processes,
            )
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(
            error
                .context()
                .entries()
                .iter()
                .find(|(key, _)| key == "cleanup_outcome")
                .map(|(_, value)| value.as_str()),
            Some("identity_mismatch")
        );
        assert_eq!(store.read(Some(task_id)).unwrap().status, "running");
        assert!(
            processes
                .read("session-provider-recovery-identity-mismatch")
                .unwrap()
                .active
        );
        assert!(handle.is_running().unwrap());
        handle.force_terminate().unwrap();
    }

    #[test]
    fn recovery_refuses_legacy_active_process_without_mutating_task_or_record() {
        let root = test_root("provider-missing-identity");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let task_id = "req-provider-recovery-missing-identity";
        store.write(&snapshot(task_id, "running")).unwrap();
        let process = provider_process(
            "session-provider-recovery-missing-identity",
            task_id,
            "none",
            None,
        );
        let mut processes = InMemoryProviderProcessTable::new();
        processes.upsert(process).unwrap();
        let process = processes
            .read("session-provider-recovery-missing-identity")
            .unwrap();

        let error = RuntimeRecoveryCoordinator
            .recover_task_store_with_provider_processes(&mut store, &mut processes)
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(
            error
                .context()
                .entries()
                .iter()
                .find(|(key, _)| key == "cleanup_outcome")
                .map(|(_, value)| value.as_str()),
            Some("missing_identity")
        );
        assert_eq!(store.read(Some(task_id)).unwrap().status, "running");
        assert_eq!(
            processes
                .read("session-provider-recovery-missing-identity")
                .unwrap(),
            process
        );
    }

    /// 返回 `snapshot` 对应的数据视图。
    fn snapshot(task_id: &str, status: &str) -> TaskStateSnapshot {
        TaskStateSnapshot {
            record_version: StateVersion::ZERO,
            owner_generation: WriterGeneration::ZERO,
            task_id: task_id.to_owned(),
            envelope: None,
            replay_delivery: None,
            status: status.to_owned(),
            attempts: 1,
            execution_owner: None,
            retry_max_attempts: 2,
            cancel_requested: false,
            cancel_accepted: false,
            cancel_reason: None,
            heartbeat_at_ms: None,
            deadline_at_ms: None,
            cancel_token: None,
            result_digest: None,
            result_size_bytes: None,
            interrupted_reason: None,
            error_kind: None,
            error_message: None,
            error_retryable: None,
            retry_ready_at_ms: None,
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
    fn recovery_envelope(
        kind: &str,
        idempotency_key: &str,
        payload: &[u8],
        max_attempts: u32,
    ) -> TaskEnvelopeSnapshot {
        TaskEnvelopeSnapshot::inline(
            kind,
            "root-agent",
            payload.to_vec(),
            idempotency_key,
            TaskAttemptPolicySnapshot::new(max_attempts, 0, None).unwrap(),
        )
        .unwrap()
    }

    fn create_and_claim_recovery_task(
        store: &mut FileSystemTaskStateStore,
        task_id: &str,
        envelope: &TaskEnvelopeSnapshot,
    ) -> eva_storage::TaskExecutionClaim {
        store
            .create(&TaskStateSnapshot::queued_with_envelope(task_id, envelope.clone()).unwrap())
            .unwrap();
        store
            .try_claim_queued(
                task_id,
                &format!("daemon.old.{task_id}"),
                &format!("cancel.old.{task_id}"),
                epoch_millis(),
            )
            .unwrap()
            .unwrap()
    }

    fn recovery_effect_intent(
        envelope: &TaskEnvelopeSnapshot,
        effect_scope: &str,
        claim: &eva_storage::TaskExecutionClaim,
    ) -> EffectLedgerIntent {
        let operation = EffectOperationIdentity::new(
            &envelope.idempotency_key,
            &envelope.kind,
            &envelope.agent_id,
            effect_scope,
            envelope.input.digest(),
        )
        .unwrap();
        EffectLedgerIntent::new(
            operation,
            claim.fence().task_id(),
            claim.fence().owner_generation(),
            claim.fence().execution_owner(),
            claim.fence().attempt(),
            claim.fence().cancel_token(),
            epoch_millis(),
        )
        .unwrap()
    }

    fn recovery_handler_registry(effect_calls: Arc<AtomicUsize>) -> TaskHandlerRegistry {
        let mut registry = TaskHandlerRegistry::with_runtime_defaults().unwrap();
        for (kind, effect_scope) in [
            ("runtime.effect.absent", "effect-scope-recovery-absent"),
            ("runtime.effect.prepared", "effect-scope-recovery-prepared"),
            (
                "runtime.effect.committed",
                "effect-scope-recovery-committed",
            ),
        ] {
            let calls = Arc::clone(&effect_calls);
            registry
                .register_non_idempotent(
                    TaskKind::parse(kind).unwrap(),
                    effect_scope,
                    move |_invocation: &TaskHandlerInvocation<'_>| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(TaskHandlerResult::new(b"effect-executed".to_vec()))
                    },
                )
                .unwrap();
        }
        registry
    }

    fn wait_for_recovery_status(
        store: &FileSystemTaskStateStore,
        task_id: &str,
        expected_status: &str,
    ) -> TaskStateSnapshot {
        let started = Instant::now();
        loop {
            let snapshot = store.read(Some(task_id)).unwrap();
            if snapshot.status == expected_status {
                return snapshot;
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "task {task_id} remained in status {} while waiting for {expected_status}",
                snapshot.status
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn epoch_millis() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_millis()
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

    fn terminated_provider_process(
        session_id: &str,
        request_id: &str,
        restart_policy: &str,
        retry_backoff_ms: Option<u64>,
    ) -> ProviderProcessSnapshot {
        let mut snapshot =
            provider_process(session_id, request_id, restart_policy, retry_backoff_ms);
        let mut command = provider_sleep_command();
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut handle = OsProcessBackend::new().spawn(command).unwrap();
        handle.identity().stamp_snapshot(&mut snapshot, 1).unwrap();
        handle.force_terminate().unwrap();
        snapshot
    }

    #[cfg(unix)]
    fn provider_sleep_command() -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        command
    }

    #[cfg(windows)]
    fn provider_sleep_command() -> Command {
        let mut command = Command::new("cmd.exe");
        command.args(["/C", "ping", "127.0.0.1", "-n", "31"]);
        command
    }

    #[cfg(not(any(unix, windows)))]
    fn provider_sleep_command() -> Command {
        Command::new("unsupported")
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
