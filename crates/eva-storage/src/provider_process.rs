//! Provider process/session table contracts.

use crate::DurableBackendLayout;
use eva_core::{AdapterId, CapabilityName, EvaError, RequestId};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "provider process/session table snapshots";

/// Queryable provider execution state shared by supervisors and future recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProcessSnapshot {
    pub session_id: String,
    pub provider_process_id: String,
    pub request_id: RequestId,
    pub adapter_id: AdapterId,
    pub capability: CapabilityName,
    pub transport: String,
    pub manifest_digest: String,
    pub start_command: String,
    pub health: String,
    pub restart_policy: String,
    pub retry_backoff_ms: Option<u64>,
    pub active: bool,
    pub last_error: Option<String>,
    pub started_at_ms: u128,
    pub updated_at_ms: u128,
    pub audit: Vec<String>,
}

/// Provider process/session table behavior required by V1.13 supervision.
pub trait ProviderProcessTable {
    fn upsert(&mut self, snapshot: ProviderProcessSnapshot) -> Result<(), EvaError>;
    fn read(&self, session_id: &str) -> Result<ProviderProcessSnapshot, EvaError>;
    fn list(&self) -> Result<Vec<ProviderProcessSnapshot>, EvaError>;
}

/// In-memory process table used by the first provider supervisor baseline.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryProviderProcessTable {
    snapshots: BTreeMap<String, ProviderProcessSnapshot>,
}

/// Filesystem-backed process table used by restart recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemProviderProcessTable {
    process_dir: PathBuf,
}

impl ProviderProcessSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn running(
        session_id: impl Into<String>,
        provider_process_id: impl Into<String>,
        request_id: RequestId,
        adapter_id: AdapterId,
        capability: CapabilityName,
        transport: impl Into<String>,
        manifest_digest: impl Into<String>,
        start_command: impl Into<String>,
        restart_policy: impl Into<String>,
    ) -> Self {
        let now = now_ms();
        let session_id = session_id.into();
        let provider_process_id = provider_process_id.into();
        Self {
            audit: vec![
                "provider.supervisor.acquired".to_owned(),
                format!("provider.session:{session_id}"),
                format!("provider.process:{provider_process_id}"),
            ],
            session_id,
            provider_process_id,
            request_id,
            adapter_id,
            capability,
            transport: transport.into(),
            manifest_digest: manifest_digest.into(),
            start_command: start_command.into(),
            health: "running".to_owned(),
            restart_policy: restart_policy.into(),
            retry_backoff_ms: None,
            active: true,
            last_error: None,
            started_at_ms: now,
            updated_at_ms: now,
        }
    }

    pub fn release(
        &mut self,
        health: impl Into<String>,
        last_error: Option<String>,
    ) -> Result<(), EvaError> {
        let health = health.into();
        if health.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "provider process health cannot be empty",
            ));
        }
        self.active = false;
        self.health = health.clone();
        self.last_error = last_error;
        self.updated_at_ms = now_ms();
        self.audit.push("provider.slot:released".to_owned());
        self.audit.push(format!("provider.health:{health}"));
        if self.last_error.is_some() {
            self.audit.push("provider.supervisor.failed".to_owned());
        } else {
            self.audit.push("provider.supervisor.completed".to_owned());
        }
        Ok(())
    }

    pub fn mark_interrupted_after_restart(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        let previous_health = self.health.clone();
        self.active = false;
        self.health = "interrupted".to_owned();
        if self.last_error.is_none() {
            self.last_error = Some(reason.clone());
        } else {
            self.audit
                .push("provider.recovery:last_error_preserved".to_owned());
        }
        self.updated_at_ms = now_ms();
        self.audit.push("provider.recovery:restart_scan".to_owned());
        self.audit.push(format!(
            "provider.recovery:previous_health:{previous_health}"
        ));
        self.audit.push(format!(
            "provider.recovery:interrupted_reason:{}",
            sanitize_audit_value(&reason)
        ));
        self.audit.push("provider.health:interrupted".to_owned());
    }

    pub fn to_storage(&self) -> String {
        let mut lines = vec![
            "version=1".to_owned(),
            format!("session_id={}", encode_field(&self.session_id)),
            format!(
                "provider_process_id={}",
                encode_field(&self.provider_process_id)
            ),
            format!("request_id={}", encode_field(self.request_id.as_str())),
            format!("adapter_id={}", encode_field(self.adapter_id.as_str())),
            format!("capability={}", encode_field(self.capability.as_str())),
            format!("transport={}", encode_field(&self.transport)),
            format!("manifest_digest={}", encode_field(&self.manifest_digest)),
            format!("start_command={}", encode_field(&self.start_command)),
            format!("health={}", encode_field(&self.health)),
            format!("restart_policy={}", encode_field(&self.restart_policy)),
            format!(
                "retry_backoff_ms={}",
                self.retry_backoff_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default()
            ),
            format!("active={}", self.active),
            format!(
                "last_error={}",
                self.last_error
                    .as_ref()
                    .map(|value| encode_field(value))
                    .unwrap_or_default()
            ),
            format!("started_at_ms={}", self.started_at_ms),
            format!("updated_at_ms={}", self.updated_at_ms),
        ];
        lines.extend(
            self.audit
                .iter()
                .map(|entry| format!("audit={}", encode_field(entry))),
        );
        lines.push(String::new());
        lines.join("\n")
    }

    pub fn from_storage(data: &str) -> Result<Self, EvaError> {
        let mut session_id = None;
        let mut provider_process_id = None;
        let mut request_id = None;
        let mut adapter_id = None;
        let mut capability = None;
        let mut transport = None;
        let mut manifest_digest = None;
        let mut start_command = None;
        let mut health = None;
        let mut restart_policy = None;
        let mut retry_backoff_ms = None;
        let mut active = None;
        let mut last_error = None;
        let mut started_at_ms = None;
        let mut updated_at_ms = None;
        let mut audit = Vec::new();

        for line in data.lines().filter(|line| !line.trim().is_empty()) {
            let Some((key, value)) = line.split_once('=') else {
                return Err(EvaError::invalid_argument(
                    "provider process snapshot is invalid",
                ));
            };
            match key {
                "version" => {
                    if value != "1" {
                        return Err(EvaError::invalid_argument(
                            "provider process version mismatch",
                        )
                        .with_context("version", value));
                    }
                }
                "session_id" => session_id = Some(decode_field(value)),
                "provider_process_id" => provider_process_id = Some(decode_field(value)),
                "request_id" => request_id = Some(RequestId::parse(&decode_field(value))?),
                "adapter_id" => adapter_id = Some(AdapterId::parse(&decode_field(value))?),
                "capability" => capability = Some(CapabilityName::parse(&decode_field(value))?),
                "transport" => transport = Some(decode_field(value)),
                "manifest_digest" => manifest_digest = Some(decode_field(value)),
                "start_command" => start_command = Some(decode_field(value)),
                "health" => health = Some(decode_field(value)),
                "restart_policy" => restart_policy = Some(decode_field(value)),
                "retry_backoff_ms" => {
                    retry_backoff_ms =
                        parse_optional_u64(value, "provider process retry_backoff_ms is invalid")?
                }
                "active" => active = Some(parse_bool(value, "active")?),
                "last_error" => last_error = decode_optional_field(value),
                "started_at_ms" => {
                    started_at_ms = Some(value.parse::<u128>().map_err(|_| {
                        EvaError::invalid_argument("provider process started_at_ms is invalid")
                    })?)
                }
                "updated_at_ms" => {
                    updated_at_ms = Some(value.parse::<u128>().map_err(|_| {
                        EvaError::invalid_argument("provider process updated_at_ms is invalid")
                    })?)
                }
                "audit" => audit.push(decode_field(value)),
                _ => {
                    return Err(EvaError::invalid_argument(
                        "provider process snapshot has unknown field",
                    )
                    .with_context("field", key));
                }
            }
        }

        Ok(Self {
            session_id: session_id
                .ok_or_else(|| EvaError::invalid_argument("provider process missing session_id"))?,
            provider_process_id: provider_process_id.ok_or_else(|| {
                EvaError::invalid_argument("provider process missing provider_process_id")
            })?,
            request_id: request_id
                .ok_or_else(|| EvaError::invalid_argument("provider process missing request_id"))?,
            adapter_id: adapter_id
                .ok_or_else(|| EvaError::invalid_argument("provider process missing adapter_id"))?,
            capability: capability
                .ok_or_else(|| EvaError::invalid_argument("provider process missing capability"))?,
            transport: transport
                .ok_or_else(|| EvaError::invalid_argument("provider process missing transport"))?,
            manifest_digest: manifest_digest.ok_or_else(|| {
                EvaError::invalid_argument("provider process missing manifest_digest")
            })?,
            start_command: start_command.ok_or_else(|| {
                EvaError::invalid_argument("provider process missing start_command")
            })?,
            health: health
                .ok_or_else(|| EvaError::invalid_argument("provider process missing health"))?,
            restart_policy: restart_policy.ok_or_else(|| {
                EvaError::invalid_argument("provider process missing restart_policy")
            })?,
            retry_backoff_ms,
            active: active
                .ok_or_else(|| EvaError::invalid_argument("provider process missing active"))?,
            last_error,
            started_at_ms: started_at_ms.ok_or_else(|| {
                EvaError::invalid_argument("provider process missing started_at_ms")
            })?,
            updated_at_ms: updated_at_ms.ok_or_else(|| {
                EvaError::invalid_argument("provider process missing updated_at_ms")
            })?,
            audit,
        })
    }
}

impl InMemoryProviderProcessTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active_for_adapter(
        &self,
        adapter_id: &AdapterId,
    ) -> Result<Vec<ProviderProcessSnapshot>, EvaError> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|snapshot| snapshot.active && &snapshot.adapter_id == adapter_id)
            .collect())
    }
}

impl FileSystemProviderProcessTable {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            process_dir: root.as_ref().join(".eva").join("provider-processes"),
        }
    }

    pub fn from_durable_layout(layout: &DurableBackendLayout) -> Self {
        Self {
            process_dir: layout.state_dir.join("provider-processes"),
        }
    }

    pub fn process_dir(&self) -> &Path {
        &self.process_dir
    }

    fn snapshot_path(&self, session_id: &str) -> Result<PathBuf, EvaError> {
        if session_id.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "provider process session id cannot be empty",
            ));
        }
        Ok(self
            .process_dir
            .join(format!("{}.provider", safe_file_segment(session_id))))
    }
}

impl ProviderProcessTable for InMemoryProviderProcessTable {
    fn upsert(&mut self, snapshot: ProviderProcessSnapshot) -> Result<(), EvaError> {
        if snapshot.session_id.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "provider process session id cannot be empty",
            ));
        }
        self.snapshots.insert(snapshot.session_id.clone(), snapshot);
        Ok(())
    }

    fn read(&self, session_id: &str) -> Result<ProviderProcessSnapshot, EvaError> {
        self.snapshots.get(session_id).cloned().ok_or_else(|| {
            EvaError::not_found("provider process session does not exist")
                .with_context("session_id", session_id)
        })
    }

    fn list(&self) -> Result<Vec<ProviderProcessSnapshot>, EvaError> {
        Ok(self.snapshots.values().cloned().collect())
    }
}

impl ProviderProcessTable for FileSystemProviderProcessTable {
    fn upsert(&mut self, snapshot: ProviderProcessSnapshot) -> Result<(), EvaError> {
        if snapshot.session_id.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "provider process session id cannot be empty",
            ));
        }
        fs::create_dir_all(&self.process_dir).map_err(|error| {
            EvaError::internal("failed to create provider process directory")
                .with_context("path", self.process_dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        fs::write(
            self.snapshot_path(&snapshot.session_id)?,
            snapshot.to_storage().as_bytes(),
        )
        .map_err(|error| {
            EvaError::internal("failed to write provider process snapshot")
                .with_context("session_id", snapshot.session_id.as_str())
                .with_context("io_error", error.to_string())
        })
    }

    fn read(&self, session_id: &str) -> Result<ProviderProcessSnapshot, EvaError> {
        let path = self.snapshot_path(session_id)?;
        let data = fs::read_to_string(&path).map_err(|error| {
            EvaError::not_found("provider process session does not exist")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        ProviderProcessSnapshot::from_storage(&data)
            .map_err(|error| error.with_context("path", path.display().to_string()))
    }

    fn list(&self) -> Result<Vec<ProviderProcessSnapshot>, EvaError> {
        let entries = match fs::read_dir(&self.process_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(
                    EvaError::internal("failed to read provider process directory")
                        .with_context("path", self.process_dir.display().to_string())
                        .with_context("io_error", error.to_string()),
                );
            }
        };

        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                EvaError::internal("failed to read provider process directory entry")
                    .with_context("path", self.process_dir.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("provider") {
                paths.push(path);
            }
        }
        paths.sort();

        paths
            .into_iter()
            .map(|path| {
                let data = fs::read_to_string(&path).map_err(|error| {
                    EvaError::internal("failed to read provider process snapshot")
                        .with_context("path", path.display().to_string())
                        .with_context("io_error", error.to_string())
                })?;
                ProviderProcessSnapshot::from_storage(&data)
                    .map_err(|error| error.with_context("path", path.display().to_string()))
            })
            .collect()
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn parse_bool(value: &str, field: &'static str) -> Result<bool, EvaError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(
            EvaError::invalid_argument("provider process boolean field is invalid")
                .with_context("field", field)
                .with_context("value", value),
        ),
    }
}

fn parse_optional_u64(value: &str, message: &'static str) -> Result<Option<u64>, EvaError> {
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse::<u64>()
            .map(Some)
            .map_err(|_| EvaError::invalid_argument(message).with_context("value", value))
    }
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

fn safe_file_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn sanitize_audit_value(value: &str) -> String {
    value.replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DurableBackendOptions, FileSystemDurableBackend};
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn snapshot(session: &str) -> ProviderProcessSnapshot {
        ProviderProcessSnapshot::running(
            session,
            format!("proc-{session}"),
            RequestId::parse("req-provider-table").unwrap(),
            AdapterId::parse("stdio-test").unwrap(),
            CapabilityName::parse("repo.analyze").unwrap(),
            "stdio",
            "fnv64:0123456789abcdef",
            "stdio-test --run",
            "none",
        )
    }

    #[test]
    fn process_table_upserts_and_lists_active_sessions() {
        let mut table = InMemoryProviderProcessTable::new();
        let adapter_id = AdapterId::parse("stdio-test").unwrap();

        table.upsert(snapshot("session-1")).unwrap();

        let sessions = table.list().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "session-1");
        assert_eq!(table.active_for_adapter(&adapter_id).unwrap().len(), 1);
    }

    #[test]
    fn process_table_release_records_last_error() {
        let mut table = InMemoryProviderProcessTable::new();
        let mut snapshot = snapshot("session-2");
        snapshot
            .release("failed", Some("provider exited before ready".to_owned()))
            .unwrap();

        table.upsert(snapshot).unwrap();
        let stored = table.read("session-2").unwrap();

        assert!(!stored.active);
        assert_eq!(stored.health, "failed");
        assert_eq!(
            stored.last_error.as_deref(),
            Some("provider exited before ready")
        );
        assert!(stored
            .audit
            .iter()
            .any(|entry| entry == "provider.slot:released"));
        assert!(stored
            .audit
            .iter()
            .any(|entry| entry == "provider.supervisor.failed"));
    }

    #[test]
    fn filesystem_process_table_survives_reopen() {
        let root = test_root("filesystem-round-trip");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let mut writer = FileSystemProviderProcessTable::from_durable_layout(backend.layout());
        let mut stored = snapshot("session-filesystem-1");
        stored.retry_backoff_ms = Some(1500);

        writer.upsert(stored.clone()).unwrap();
        let reader = FileSystemProviderProcessTable::from_durable_layout(backend.layout());
        let by_id = reader.read("session-filesystem-1").unwrap();
        let listed = reader.list().unwrap();

        assert_eq!(by_id, stored);
        assert_eq!(listed, vec![stored]);
        assert!(reader
            .process_dir()
            .join("session-filesystem-1.provider")
            .is_file());
    }

    #[test]
    fn interrupted_provider_process_preserves_last_error_and_audit_chain() {
        let mut stored = snapshot("session-interrupted");
        stored.last_error = Some("provider stderr: safe error".to_owned());
        let original_audit_len = stored.audit.len();

        stored.mark_interrupted_after_restart("daemon restart interrupted active provider session");

        assert!(!stored.active);
        assert_eq!(stored.health, "interrupted");
        assert_eq!(
            stored.last_error.as_deref(),
            Some("provider stderr: safe error")
        );
        assert!(stored.audit.len() > original_audit_len);
        assert!(stored
            .audit
            .iter()
            .any(|entry| entry == "provider.recovery:last_error_preserved"));
        assert!(stored
            .audit
            .iter()
            .any(|entry| entry == "provider.health:interrupted"));
    }

    #[test]
    fn filesystem_process_table_reports_corrupt_snapshot() {
        let root = test_root("filesystem-corrupt");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let table = FileSystemProviderProcessTable::from_durable_layout(backend.layout());
        fs::create_dir_all(table.process_dir()).unwrap();
        fs::write(
            table.process_dir().join("corrupt.provider"),
            "version=1\nsession_id=session-corrupt\n",
        )
        .unwrap();

        let error = table.list().unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
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
                "eva-storage-provider-process-{name}-{}-{now}",
                std::process::id()
            )),
        }
    }
}
