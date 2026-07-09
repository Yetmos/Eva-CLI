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
    pub heartbeat_at_ms: Option<u128>,
    pub deadline_at_ms: Option<u128>,
    pub cancel_token: Option<String>,
    pub interrupted_reason: Option<String>,
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
    pub fn queued(task_id: impl Into<String>) -> Result<Self, EvaError> {
        let task_id = task_id.into();
        RequestId::parse(&task_id)?;
        Ok(Self {
            task_id,
            status: "queued".to_owned(),
            attempts: 0,
            retry_max_attempts: 1,
            cancel_requested: false,
            cancel_accepted: false,
            cancel_reason: None,
            heartbeat_at_ms: None,
            deadline_at_ms: None,
            cancel_token: None,
            interrupted_reason: None,
            error_kind: None,
            error_message: None,
            logs: Vec::new(),
            dead_letters: Vec::new(),
            replayed_events: Vec::new(),
        })
    }

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
                "heartbeat_at_ms={}",
                self.heartbeat_at_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default()
            ),
            format!(
                "deadline_at_ms={}",
                self.deadline_at_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default()
            ),
            format!(
                "cancel_token={}",
                self.cancel_token
                    .as_ref()
                    .map(|value| encode_field(value))
                    .unwrap_or_default()
            ),
            format!(
                "interrupted_reason={}",
                self.interrupted_reason
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
            heartbeat_at_ms: None,
            deadline_at_ms: None,
            cancel_token: None,
            interrupted_reason: None,
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
            } else if let Some(value) = line.strip_prefix("heartbeat_at_ms=") {
                snapshot.heartbeat_at_ms = parse_optional_stored_u128("heartbeat_at_ms", value)?;
            } else if let Some(value) = line.strip_prefix("deadline_at_ms=") {
                snapshot.deadline_at_ms = parse_optional_stored_u128("deadline_at_ms", value)?;
            } else if let Some(value) = line.strip_prefix("cancel_token=") {
                snapshot.cancel_token = decode_optional_field(value);
            } else if let Some(value) = line.strip_prefix("interrupted_reason=") {
                snapshot.interrupted_reason = decode_optional_field(value);
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

    pub fn mark_running(
        &mut self,
        heartbeat_at_ms: u128,
        deadline_at_ms: Option<u128>,
        cancel_token: impl Into<String>,
    ) {
        self.status = "running".to_owned();
        self.heartbeat_at_ms = Some(heartbeat_at_ms);
        self.deadline_at_ms = deadline_at_ms;
        self.cancel_token = Some(cancel_token.into());
        self.push_log("info", "task marked running");
    }

    pub fn record_heartbeat(&mut self, heartbeat_at_ms: u128) {
        self.heartbeat_at_ms = Some(heartbeat_at_ms);
        self.push_log("info", format!("task heartbeat at {heartbeat_at_ms}"));
    }

    pub fn request_cancel(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.cancel_requested = true;
        self.cancel_reason = Some(reason.clone());
        if self.is_terminal() {
            self.cancel_accepted = false;
            self.push_log(
                "warning",
                "cancel requested after task reached a terminal state",
            );
        } else {
            self.cancel_accepted = true;
            self.status = "cancelling".to_owned();
            self.push_log("warning", format!("cancel requested: {reason}"));
        }
    }

    pub fn mark_cancelled(&mut self) {
        self.status = "cancelled".to_owned();
        self.cancel_requested = true;
        self.cancel_accepted = true;
        self.push_log("warning", "task marked cancelled");
    }

    pub fn mark_timed_out(&mut self, now_ms: u128) {
        self.status = "timed_out".to_owned();
        self.heartbeat_at_ms = Some(now_ms);
        self.error_kind = Some("timeout".to_owned());
        self.error_message = Some("task deadline exceeded".to_owned());
        self.push_log("error", format!("task timed out at {now_ms}"));
    }

    pub fn mark_completed(&mut self, attempts: usize) {
        self.status = "completed".to_owned();
        self.attempts = attempts;
        self.error_kind = None;
        self.error_message = None;
        self.push_log("info", "task completed");
    }

    pub fn mark_failed(
        &mut self,
        attempts: usize,
        error_kind: impl Into<String>,
        error_message: impl Into<String>,
    ) {
        self.status = "failed".to_owned();
        self.attempts = attempts;
        self.error_kind = Some(error_kind.into());
        self.error_message = Some(error_message.into());
        self.push_log("error", "task failed");
    }

    pub fn mark_interrupted(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.status = "interrupted".to_owned();
        self.interrupted_reason = Some(reason.clone());
        self.push_log("warning", format!("task interrupted: {reason}"));
    }

    pub fn mark_recovering(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.status = "recovering".to_owned();
        self.interrupted_reason = Some(reason.clone());
        self.push_log("warning", format!("task recovering: {reason}"));
    }

    pub fn deadline_expired(&self, now_ms: u128) -> bool {
        self.deadline_at_ms
            .map(|deadline| now_ms >= deadline)
            .unwrap_or(false)
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "completed" | "failed" | "cancelled" | "timed_out" | "interrupted"
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

    pub fn list_snapshots(&self) -> Result<Vec<TaskStateSnapshot>, EvaError> {
        let dir = self.task_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(EvaError::internal("failed to read task state directory")
                    .with_context("path", dir.display().to_string())
                    .with_context("io_error", error.to_string()));
            }
        };

        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                EvaError::internal("failed to read task state directory entry")
                    .with_context("path", dir.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
            let path = entry.path();
            if path.file_name().and_then(|name| name.to_str()) == Some("latest-basic.task") {
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) == Some("task") {
                paths.push(path);
            }
        }
        paths.sort();

        paths
            .into_iter()
            .map(|path| {
                let data = fs::read_to_string(&path).map_err(|error| {
                    EvaError::internal("failed to read task state")
                        .with_context("path", path.display().to_string())
                        .with_context("io_error", error.to_string())
                })?;
                TaskStateSnapshot::from_storage(&data)
                    .map_err(|error| error.with_context("path", path.display().to_string()))
            })
            .collect()
    }

    pub fn update_snapshot<F>(
        &mut self,
        task_id: &str,
        update: F,
    ) -> Result<TaskStateSnapshot, EvaError>
    where
        F: FnOnce(&mut TaskStateSnapshot) -> Result<(), EvaError>,
    {
        let mut snapshot = self.read(Some(task_id))?;
        update(&mut snapshot)?;
        self.write(&snapshot)?;
        Ok(snapshot)
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

fn parse_optional_stored_u128(name: &'static str, value: &str) -> Result<Option<u128>, EvaError> {
    if value.is_empty() {
        return Ok(None);
    }
    value.parse::<u128>().map(Some).map_err(|_| {
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
    fn filesystem_task_state_lists_snapshots_without_latest_duplicate() {
        let root = test_root("list");
        let mut writer = FileSystemTaskStateStore::new(root.path());
        writer
            .write(&sample_snapshot("req-task-state-list-2"))
            .unwrap();
        writer
            .write(&sample_snapshot("req-task-state-list-1"))
            .unwrap();

        let snapshots = writer.list_snapshots().unwrap();

        assert_eq!(
            snapshots
                .iter()
                .map(|snapshot| snapshot.task_id.as_str())
                .collect::<Vec<_>>(),
            vec!["req-task-state-list-1", "req-task-state-list-2"]
        );
    }

    #[test]
    fn filesystem_task_state_lists_empty_missing_directory() {
        let root = test_root("list-missing");
        let store = FileSystemTaskStateStore::new(root.path());

        assert!(store.list_snapshots().unwrap().is_empty());
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

    #[test]
    fn interrupted_task_state_is_terminal() {
        let mut snapshot = sample_snapshot("req-task-state-interrupted");
        snapshot.status = "interrupted".to_owned();

        assert!(snapshot.is_terminal());
    }

    #[test]
    fn task_lifecycle_tracks_heartbeat_deadline_cancel_and_timeout() {
        let mut snapshot = TaskStateSnapshot::queued("req-task-lifecycle").unwrap();

        snapshot.mark_running(100, Some(200), "cancel-token-1");
        snapshot.record_heartbeat(150);
        snapshot.request_cancel("operator requested stop");

        assert_eq!(snapshot.status, "cancelling");
        assert_eq!(snapshot.heartbeat_at_ms, Some(150));
        assert_eq!(snapshot.deadline_at_ms, Some(200));
        assert_eq!(snapshot.cancel_token.as_deref(), Some("cancel-token-1"));
        assert!(snapshot.cancel_requested);
        assert!(snapshot.cancel_accepted);
        assert!(!snapshot.deadline_expired(199));
        assert!(snapshot.deadline_expired(200));

        snapshot.mark_timed_out(250);

        assert_eq!(snapshot.status, "timed_out");
        assert!(snapshot.is_terminal());
        assert_eq!(snapshot.error_kind.as_deref(), Some("timeout"));
        assert!(snapshot.logs.iter().any(|entry| entry.level == "error"));
    }

    #[test]
    fn filesystem_task_state_update_appends_lifecycle_log() {
        let root = test_root("lifecycle-update");
        let mut store = FileSystemTaskStateStore::new(root.path());
        let mut snapshot = TaskStateSnapshot::queued("req-task-lifecycle-store").unwrap();
        snapshot.mark_running(10, Some(20), "cancel-token-store");
        store.write(&snapshot).unwrap();

        let updated = store
            .update_snapshot("req-task-lifecycle-store", |snapshot| {
                snapshot.request_cancel("operator cancel");
                Ok(())
            })
            .unwrap();

        assert_eq!(updated.status, "cancelling");
        assert_eq!(updated.logs.len(), 2);
        let reread = store.read(Some("req-task-lifecycle-store")).unwrap();
        assert_eq!(reread.status, "cancelling");
        assert_eq!(reread.cancel_reason.as_deref(), Some("operator cancel"));
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
            heartbeat_at_ms: None,
            deadline_at_ms: None,
            cancel_token: None,
            interrupted_reason: None,
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
