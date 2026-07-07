//! V1.6.4 runtime crash recovery coordinator.

use eva_storage::{FileSystemTaskStateStore, TaskStateSnapshot, TaskStateStore};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "runtime restart recovery over durable task evidence";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RuntimeRecoveryCoordinator;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRecoveryReport {
    pub scanned_tasks: usize,
    pub recovered_tasks: Vec<RecoveredTask>,
    pub unchanged_tasks: Vec<String>,
    pub recovered_snapshots: Vec<TaskStateSnapshot>,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredTask {
    pub task_id: String,
    pub previous_status: String,
    pub status: String,
    pub redrive_candidate: bool,
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
            audit,
        }
    }

    pub fn recover_task_store(
        &self,
        store: &mut FileSystemTaskStateStore,
    ) -> Result<RuntimeRecoveryReport, eva_core::EvaError> {
        let mut report = self.recover_snapshots(store.list_snapshots()?);
        for snapshot in &report.recovered_snapshots {
            store.write(snapshot)?;
        }
        report.audit.push(format!(
            "runtime.recovery:persisted:{}",
            report.recovered_snapshots.len()
        ));
        Ok(report)
    }
}

fn recovery_status(snapshot: &TaskStateSnapshot) -> Option<&'static str> {
    match snapshot.status.as_str() {
        "queued" | "running" if !snapshot.dead_letters.is_empty() => Some("recovering"),
        "queued" | "running" => Some("interrupted"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_storage::{TaskStateDeadLetterSnapshot, TaskStateLogSnapshot};
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

    fn snapshot(task_id: &str, status: &str) -> TaskStateSnapshot {
        TaskStateSnapshot {
            task_id: task_id.to_owned(),
            status: status.to_owned(),
            attempts: 1,
            retry_max_attempts: 2,
            cancel_requested: false,
            cancel_accepted: false,
            cancel_reason: None,
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
