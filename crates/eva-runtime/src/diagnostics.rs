//! 中文：V1.6.5 冒烟门禁使用的持久化运行时诊断。
//! Durable runtime diagnostics for V1.6.5 smoke gates.

use eva_core::EvaError;
use eva_eventbus::DurableEventBus;
use eva_storage::{
    migration_lock_is_held, DurableBackend, DurableBackendOptions, EventLogStatus,
    FileSystemDurableBackend,
};
use std::path::Path;

/// 中文：本模块以只读方式验证持久化后端并统计可安全重驱的死信。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "durable backend diagnostics for runtime smoke gates";

/// 中文：持久化诊断扫描使用的时间边界选项。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DurableDiagnosticsOptions {
    /// 中文：判定死信重驱是否到期的相对毫秒阈值。
    pub redrive_ready_at_ms: u64,
}

/// 中文：持久化布局、迁移状态、事件日志和待重驱数量的只读报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableDiagnosticsReport {
    /// 中文：已检查后端的规范化根路径。
    pub backend_path: String,
    /// 中文：验证时使用的只读后端模式。
    pub backend_mode: String,
    /// 中文：当前持久化 Schema 版本。
    pub schema_version: u32,
    /// 中文：磁盘目录布局版本。
    pub layout_version: String,
    /// 中文：迁移锁对应的稳定状态文本。
    pub migration_status: String,
    /// 中文：是否存在迁移锁文件。
    pub migration_locked: bool,
    /// 中文：事件日志中的记录总数。
    pub event_log_records: usize,
    /// 中文：持久化死信记录总数。
    pub dead_letter_count: usize,
    /// 中文：已到期、未确认且没有重放记录的死信数量。
    pub pending_redrive_count: usize,
}

/// 中文：以只读模式打开后端，验证布局并生成不会修改磁盘的诊断报告。
///
/// 待重驱统计只包含已经进入事件日志、状态仍为追加或失败、达到时间阈值且没有任何
/// 重放记录的死信；已确认或已有重放的事件不会被重复计入调度候选。
pub fn inspect_durable_backend(
    root: impl AsRef<Path>,
    options: DurableDiagnosticsOptions,
) -> Result<DurableDiagnosticsReport, EvaError> {
    let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_only(root.as_ref()))?;
    let backend_report = backend.verify()?;
    let bus = DurableEventBus::open_read_only(backend.layout())?;
    let migration_locked = migration_lock_is_held(backend.layout())?;
    let pending_redrive_count = bus
        .dead_letters()
        .iter()
        .filter(|record| {
            record.redrive.next_attempt_after_ms <= options.redrive_ready_at_ms
                && matches!(
                    bus.event_log_status(record.event_id()),
                    Some(EventLogStatus::Appended | EventLogStatus::Failed)
                )
                && bus.latest_replay_record(record.event_id()).is_none()
        })
        .count();

    Ok(DurableDiagnosticsReport {
        backend_path: backend_report.root,
        backend_mode: backend_report.mode,
        schema_version: backend_report.schema_version,
        layout_version: backend_report.layout_version,
        migration_status: if migration_locked { "locked" } else { "idle" }.to_owned(),
        migration_locked,
        event_log_records: bus.log().records().len(),
        dead_letter_count: bus.dead_letters().len(),
        pending_redrive_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::{AgentId, EvaError, Event, EventPayload, Topic};
    use eva_eventbus::{DurableEventBus, EventBus, RedrivePolicy};
    use eva_storage::{DurableBackendOptions, FileSystemDurableBackend};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    /// 中文：验证空后端报告版本信息且只读诊断不会创建事件目录。
    fn durable_diagnostics_report_backend_and_empty_redrive_count() {
        let root = test_root("empty");
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let _ = backend.verify().unwrap();
        }

        let report =
            inspect_durable_backend(root.path(), DurableDiagnosticsOptions::default()).unwrap();

        assert_eq!(report.backend_mode, "read_only");
        assert_eq!(
            report.schema_version,
            eva_storage::CURRENT_DURABLE_SCHEMA_VERSION
        );
        assert_eq!(report.layout_version, eva_storage::DURABLE_LAYOUT_VERSION);
        assert_eq!(report.migration_status, "idle");
        assert!(!report.migration_locked);
        assert_eq!(report.event_log_records, 0);
        assert_eq!(report.dead_letter_count, 0);
        assert_eq!(report.pending_redrive_count, 0);
        assert!(report
            .backend_path
            .contains("eva-runtime-durable-diagnostics-empty"));
        assert!(!root.path().join("events").join("log").exists());
        assert!(!root.path().join("events").join("dead_letters").exists());
    }

    #[test]
    /// 中文：验证统计只包含到期、未确认且尚未重放的死信。
    fn durable_diagnostics_counts_only_due_unacked_redrive_candidates() {
        let root = test_root("pending-redrive");
        {
            let backend =
                FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
                    .unwrap();
            let mut bus = DurableEventBus::open(backend.layout()).unwrap();
            let due = event("evt-diagnostics-due");
            let acked = event("evt-diagnostics-acked");
            let not_due = event("evt-diagnostics-not-due");
            let already_redriven = event("evt-diagnostics-already-redriven");

            bus.publish(due.clone()).unwrap();
            bus.dead_letter(due, EvaError::timeout("handler timeout"))
                .unwrap();

            bus.publish(acked.clone()).unwrap();
            bus.dead_letter(acked.clone(), EvaError::timeout("handler timeout"))
                .unwrap();
            bus.ack(acked.event_id(), AgentId::parse("agent-a").unwrap())
                .unwrap();

            bus.publish(not_due.clone()).unwrap();
            bus.dead_letter(not_due.clone(), EvaError::timeout("handler timeout"))
                .unwrap();
            bus.set_dead_letter_redrive_policy(
                not_due.event_id(),
                RedrivePolicy {
                    retry_delay_ms: 5_000,
                    next_attempt_after_ms: 5_000,
                },
            )
            .unwrap();

            bus.publish(already_redriven.clone()).unwrap();
            bus.dead_letter(
                already_redriven.clone(),
                EvaError::timeout("handler timeout"),
            )
            .unwrap();
            let receipt = bus
                .redrive_dead_letter(already_redriven.event_id())
                .unwrap();
            bus.ack(
                &receipt.event_id,
                AgentId::parse("scheduler-retry").unwrap(),
            )
            .unwrap();
        }

        let report = inspect_durable_backend(
            root.path(),
            DurableDiagnosticsOptions {
                redrive_ready_at_ms: 1_000,
            },
        )
        .unwrap();

        assert_eq!(report.event_log_records, 5);
        assert_eq!(report.dead_letter_count, 4);
        assert_eq!(report.pending_redrive_count, 1);
    }

    /// 中文：构造持久化诊断测试使用的文本事件。
    fn event(event_id: &str) -> Event {
        Event::new(
            eva_core::EventId::parse(event_id).unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::Text("hello".to_owned()),
        )
    }

    /// 中文：创建按进程和纳秒隔离的临时测试根目录。
    fn test_root(name: &str) -> TestRoot {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "eva-runtime-durable-diagnostics-{name}-{}-{now}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        TestRoot { path }
    }

    /// 中文：测试临时目录所有者，离开作用域时自动清理。
    struct TestRoot {
        /// 中文：临时后端根路径。
        path: PathBuf,
    }

    impl TestRoot {
        /// 中文：返回临时根目录路径。
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestRoot {
        /// 中文：尽力删除测试目录，清理失败不覆盖原测试结果。
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
