//! Durable runtime diagnostics for V1.6.5 smoke gates.

use eva_core::{EvaError, EventId};
use eva_eventbus::DurableEventBus;
use eva_storage::{
    DurableBackend, DurableBackendOptions, EventLogStatus, FileSystemDurableBackend,
};
use std::path::Path;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "durable backend diagnostics for runtime smoke gates";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DurableDiagnosticsOptions {
    pub redrive_ready_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableDiagnosticsReport {
    pub backend_path: String,
    pub backend_mode: String,
    pub schema_version: u32,
    pub layout_version: String,
    pub migration_status: String,
    pub migration_locked: bool,
    pub event_log_records: usize,
    pub dead_letter_count: usize,
    pub pending_redrive_count: usize,
}

pub fn inspect_durable_backend(
    root: impl AsRef<Path>,
    options: DurableDiagnosticsOptions,
) -> Result<DurableDiagnosticsReport, EvaError> {
    let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_only(root.as_ref()))?;
    let backend_report = backend.verify()?;
    let bus = DurableEventBus::open_read_only(backend.layout())?;
    let migration_locked = backend.layout().migration_lock_path.exists();
    let pending_redrive_count = bus
        .dead_letters()
        .iter()
        .filter(|record| {
            record.redrive.next_attempt_after_ms <= options.redrive_ready_at_ms
                && matches!(
                    event_log_status(&bus, record.event_id()),
                    Some(EventLogStatus::Appended | EventLogStatus::Failed)
                )
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

fn event_log_status(bus: &DurableEventBus, event_id: &EventId) -> Option<EventLogStatus> {
    bus.log()
        .records()
        .iter()
        .find(|record| record.event.event_id() == event_id)
        .map(|record| record.status)
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
        }

        let report = inspect_durable_backend(
            root.path(),
            DurableDiagnosticsOptions {
                redrive_ready_at_ms: 1_000,
            },
        )
        .unwrap();

        assert_eq!(report.event_log_records, 3);
        assert_eq!(report.dead_letter_count, 3);
        assert_eq!(report.pending_redrive_count, 1);
    }

    fn event(event_id: &str) -> Event {
        Event::new(
            eva_core::EventId::parse(event_id).unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::Text("hello".to_owned()),
        )
    }

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
}
