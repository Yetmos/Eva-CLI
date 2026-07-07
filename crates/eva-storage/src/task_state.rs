//! Durable task state contracts and filesystem implementation.

use crate::DurableBackendLayout;
use eva_core::{EvaError, RequestId};
use std::fs;
use std::path::{Path, PathBuf};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "durable task state interfaces and process-boundary snapshots";

/// Stored task summary used by CLI task commands across process boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStateSnapshot {
    pub task_id: String,
    pub status: String,
    pub attempts: usize,
    pub retry_max_attempts: usize,
    pub cancel_requested: bool,
    pub cancel_accepted: bool,
    pub cancel_reason: Option<String>,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
    pub logs: Vec<TaskStateLogSnapshot>,
    pub dead_letters: Vec<TaskStateDeadLetterSnapshot>,
    pub replayed_events: Vec<TaskStateReplaySnapshot>,
}

/// Stored task log entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStateLogSnapshot {
    pub sequence: u64,
    pub level: String,
    pub message: String,
}

/// Stored dead-letter summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStateDeadLetterSnapshot {
    pub event_id: String,
    pub topic: String,
    pub reason_kind: String,
    pub reason: String,
    pub replay_count: usize,
}

/// Stored replay summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStateReplaySnapshot {
    pub event_id: String,
    pub sequence: u64,
    pub topic: String,
}

/// Durable task state behavior required by CLI/runtime boundaries.
pub trait TaskStateStore {
    fn write(&mut self, snapshot: &TaskStateSnapshot) -> Result<(), EvaError>;
    fn read(&self, task_id: Option<&str>) -> Result<TaskStateSnapshot, EvaError>;
}

/// Filesystem-backed task state store that preserves the existing `.eva/tasks` layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemTaskStateStore {
    project_root: PathBuf,
    task_dir: PathBuf,
}

impl TaskStateSnapshot {
    pub fn to_storage(&self) -> String {
        let mut lines = vec![
            format!("task_id={}", encode_field(&self.task_id)),
            format!("status={}", encode_field(&self.status)),
            format!("attempts={}", self.attempts),
            format!("retry_max_attempts={}", self.retry_max_attempts),
            format!("cancel_requested={}", self.cancel_requested),
            format!("cancel_accepted={}", self.cancel_accepted),
            format!(
                "cancel_reason={}",
                self.cancel_reason
                    .as_ref()
                    .map(|value| encode_field(value))
                    .unwrap_or_default()
            ),
            format!(
                "error_kind={}",
                self.error_kind
                    .as_ref()
                    .map(|value| encode_field(value))
                    .unwrap_or_default()
            ),
            format!(
                "error_message={}",
                self.error_message
                    .as_ref()
                    .map(|value| encode_field(value))
                    .unwrap_or_default()
            ),
        ];
        lines.extend(self.logs.iter().map(|entry| {
            format!(
                "log={}|{}|{}",
                entry.sequence,
                encode_field(&entry.level),
                encode_field(&entry.message)
            )
        }));
        lines.extend(self.dead_letters.iter().map(|entry| {
            format!(
                "dead_letter={}|{}|{}|{}|{}",
                encode_field(&entry.event_id),
                encode_field(&entry.topic),
                encode_field(&entry.reason_kind),
                encode_field(&entry.reason),
                entry.replay_count
            )
        }));
        lines.extend(self.replayed_events.iter().map(|entry| {
            format!(
                "replay={}|{}|{}",
                encode_field(&entry.event_id),
                entry.sequence,
                encode_field(&entry.topic)
            )
        }));
        lines.push(String::new());
        lines.join("\n")
    }

    pub fn from_storage(data: &str) -> Result<Self, EvaError> {
        let mut snapshot = Self {
            task_id: String::new(),
            status: String::new(),
            attempts: 0,
            retry_max_attempts: 1,
            cancel_requested: false,
            cancel_accepted: false,
            cancel_reason: None,
            error_kind: None,
            error_message: None,
            logs: Vec::new(),
            dead_letters: Vec::new(),
            replayed_events: Vec::new(),
        };

        for line in data.lines().filter(|line| !line.trim().is_empty()) {
            if let Some(value) = line.strip_prefix("task_id=") {
                snapshot.task_id = decode_field(value);
            } else if let Some(value) = line.strip_prefix("status=") {
                snapshot.status = decode_field(value);
            } else if let Some(value) = line.strip_prefix("attempts=") {
                snapshot.attempts = parse_stored_usize("attempts", value)?;
            } else if let Some(value) = line.strip_prefix("retry_max_attempts=") {
                snapshot.retry_max_attempts = parse_stored_usize("retry_max_attempts", value)?;
            } else if let Some(value) = line.strip_prefix("cancel_requested=") {
                snapshot.cancel_requested = value == "true";
            } else if let Some(value) = line.strip_prefix("cancel_accepted=") {
                snapshot.cancel_accepted = value == "true";
            } else if let Some(value) = line.strip_prefix("cancel_reason=") {
                snapshot.cancel_reason = decode_optional_field(value);
            } else if let Some(value) = line.strip_prefix("error_kind=") {
                snapshot.error_kind = decode_optional_field(value);
            } else if let Some(value) = line.strip_prefix("error_message=") {
                snapshot.error_message = decode_optional_field(value);
            } else if let Some(value) = line.strip_prefix("log=") {
                let parts = split_stored_fields(value, 3, "log")?;
                snapshot.logs.push(TaskStateLogSnapshot {
                    sequence: parse_stored_u64("log.sequence", &parts[0])?,
                    level: decode_field(&parts[1]),
                    message: decode_field(&parts[2]),
                });
            } else if let Some(value) = line.strip_prefix("dead_letter=") {
                let parts = split_stored_fields(value, 5, "dead_letter")?;
                snapshot.dead_letters.push(TaskStateDeadLetterSnapshot {
                    event_id: decode_field(&parts[0]),
                    topic: decode_field(&parts[1]),
                    reason_kind: decode_field(&parts[2]),
                    reason: decode_field(&parts[3]),
                    replay_count: parse_stored_usize("dead_letter.replay_count", &parts[4])?,
                });
            } else if let Some(value) = line.strip_prefix("replay=") {
                let parts = split_stored_fields(value, 3, "replay")?;
                snapshot.replayed_events.push(TaskStateReplaySnapshot {
                    event_id: decode_field(&parts[0]),
                    sequence: parse_stored_u64("replay.sequence", &parts[1])?,
                    topic: decode_field(&parts[2]),
                });
            }
        }

        if snapshot.task_id.is_empty() || snapshot.status.is_empty() {
            return Err(EvaError::invalid_argument("task state file is incomplete"));
        }
        Ok(snapshot)
    }

    pub fn push_log(&mut self, level: impl Into<String>, message: impl Into<String>) {
        self.logs.push(TaskStateLogSnapshot {
            sequence: self.logs.len() as u64 + 1,
            level: level.into(),
            message: message.into(),
        });
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "completed" | "failed" | "cancelled" | "timed_out"
        )
    }
}

impl FileSystemTaskStateStore {
    pub fn new(project_root: impl AsRef<Path>) -> Self {
        let project_root = project_root.as_ref().to_path_buf();
        let task_dir = project_root.join(".eva").join("tasks");
        Self {
            project_root,
            task_dir,
        }
    }

    pub fn from_durable_layout(layout: &DurableBackendLayout) -> Self {
        Self {
            project_root: layout.root.clone(),
            task_dir: layout.task_dir.clone(),
        }
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub fn task_dir(&self) -> PathBuf {
        self.task_dir.clone()
    }

    fn latest_task_path(&self) -> PathBuf {
        self.task_dir().join("latest-basic.task")
    }

    fn task_path(&self, task_id: &str) -> Result<PathBuf, EvaError> {
        RequestId::parse(task_id)?;
        Ok(self.task_dir().join(format!("{task_id}.task")))
    }
}

impl TaskStateStore for FileSystemTaskStateStore {
    fn write(&mut self, snapshot: &TaskStateSnapshot) -> Result<(), EvaError> {
        RequestId::parse(&snapshot.task_id)?;
        let dir = self.task_dir();
        fs::create_dir_all(&dir).map_err(|error| {
            EvaError::internal("failed to create task state directory")
                .with_context("path", dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let data = snapshot.to_storage();
        fs::write(self.task_path(&snapshot.task_id)?, data.as_bytes()).map_err(|error| {
            EvaError::internal("failed to write task state")
                .with_context("task_id", snapshot.task_id.as_str())
                .with_context("io_error", error.to_string())
        })?;
        fs::write(self.latest_task_path(), data.as_bytes()).map_err(|error| {
            EvaError::internal("failed to write latest task state")
                .with_context("task_id", snapshot.task_id.as_str())
                .with_context("io_error", error.to_string())
        })
    }

    fn read(&self, task_id: Option<&str>) -> Result<TaskStateSnapshot, EvaError> {
        let path = match task_id {
            Some(task_id) => self.task_path(task_id)?,
            None => self.latest_task_path(),
        };
        let data = fs::read_to_string(&path).map_err(|error| {
            EvaError::not_found("task state does not exist")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
                .with_context("suggestion", "run `eva run --example basic` first")
        })?;
        TaskStateSnapshot::from_storage(&data)
    }
}

fn split_stored_fields(
    value: &str,
    expected: usize,
    field: &'static str,
) -> Result<Vec<String>, EvaError> {
    let parts = value.split('|').map(str::to_owned).collect::<Vec<_>>();
    if parts.len() != expected {
        return Err(
            EvaError::invalid_argument("task state field has invalid arity")
                .with_context("field", field)
                .with_context("expected", expected.to_string())
                .with_context("actual", parts.len().to_string()),
        );
    }
    Ok(parts)
}

fn parse_stored_usize(name: &'static str, value: &str) -> Result<usize, EvaError> {
    value.parse::<usize>().map_err(|_| {
        EvaError::invalid_argument("stored task field is not an unsigned integer")
            .with_context("field", name)
            .with_context("value", value)
    })
}

fn parse_stored_u64(name: &'static str, value: &str) -> Result<u64, EvaError> {
    value.parse::<u64>().map_err(|_| {
        EvaError::invalid_argument("stored task field is not an unsigned integer")
            .with_context("field", name)
            .with_context("value", value)
    })
}

fn decode_optional_field(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(decode_field(value))
    }
}

fn encode_field(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\n', "%0A")
        .replace('\r', "%0D")
        .replace('\t', "%09")
        .replace('|', "%7C")
        .replace('=', "%3D")
}

fn decode_field(value: &str) -> String {
    value
        .replace("%0A", "\n")
        .replace("%0D", "\r")
        .replace("%09", "\t")
        .replace("%7C", "|")
        .replace("%3D", "=")
        .replace("%25", "%")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn filesystem_task_state_survives_store_recreation() {
        let root = test_root("round-trip");
        let mut writer = FileSystemTaskStateStore::new(root.path());
        let snapshot = sample_snapshot("req-task-state-1");

        writer.write(&snapshot).unwrap();
        let reader = FileSystemTaskStateStore::new(root.path());
        let by_id = reader.read(Some("req-task-state-1")).unwrap();
        let latest = reader.read(None).unwrap();

        assert_eq!(by_id, snapshot);
        assert_eq!(latest, snapshot);
        assert_eq!(reader.project_root(), root.path());
    }

    #[test]
    fn filesystem_task_state_updates_cancel_log_across_process_boundary() {
        let root = test_root("cancel");
        let mut writer = FileSystemTaskStateStore::new(root.path());
        let mut snapshot = sample_snapshot("req-task-state-2");
        writer.write(&snapshot).unwrap();

        let reader = FileSystemTaskStateStore::new(root.path());
        snapshot = reader.read(Some("req-task-state-2")).unwrap();
        snapshot.cancel_requested = true;
        snapshot.cancel_accepted = false;
        snapshot.cancel_reason = Some("too late".to_owned());
        snapshot.push_log("warning", "cancel requested after terminal state");
        writer.write(&snapshot).unwrap();

        let updated = reader.read(None).unwrap();

        assert!(updated.cancel_requested);
        assert_eq!(updated.cancel_reason.as_deref(), Some("too late"));
        assert_eq!(updated.logs.last().unwrap().level, "warning");
    }

    #[test]
    fn filesystem_task_state_can_use_durable_backend_layout() {
        let root = test_root("durable-layout");
        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let mut writer = FileSystemTaskStateStore::from_durable_layout(backend.layout());
        let snapshot = sample_snapshot("req-task-state-durable-1");

        writer.write(&snapshot).unwrap();
        let reader = FileSystemTaskStateStore::from_durable_layout(backend.layout());
        let by_id = reader.read(Some("req-task-state-durable-1")).unwrap();

        assert_eq!(by_id, snapshot);
        assert_eq!(reader.task_dir(), backend.layout().task_dir);
        assert!(backend
            .layout()
            .task_dir
            .join("req-task-state-durable-1.task")
            .is_file());
    }

    #[test]
    fn missing_task_state_is_not_found() {
        let root = test_root("missing");
        let store = FileSystemTaskStateStore::new(root.path());

        let error = store.read(Some("req-missing-task")).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::NotFound);
    }

    #[test]
    fn invalid_task_state_rejects_incomplete_files() {
        let error = TaskStateSnapshot::from_storage("task_id=req-only\n").unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
    }

    fn sample_snapshot(task_id: &str) -> TaskStateSnapshot {
        TaskStateSnapshot {
            task_id: task_id.to_owned(),
            status: "completed".to_owned(),
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
                message: "event accepted: evt-basic-1".to_owned(),
            }],
            dead_letters: Vec::new(),
            replayed_events: vec![TaskStateReplaySnapshot {
                event_id: "evt-basic-1".to_owned(),
                sequence: 1,
                topic: "/input/user".to_owned(),
            }],
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
                "eva-storage-task-state-{name}-{}-{now}",
                std::process::id()
            )),
        }
    }
}
