//! Durable backend 的 schema、目录布局、迁移互斥与长期 writer ownership。
//! Durable backend schema, layout, migration exclusion, and writer ownership.

use eva_core::EvaError;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// 本模块的架构职责：定义持久化根布局，短暂保护迁移，并提供长期 fenced writer ownership。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "durable backend schema, layout, migration locks, and fenced writer ownership";

/// 当前可读写的 manifest schema 版本；不匹配时拒绝打开，避免无迁移器的隐式升级。
pub const CURRENT_DURABLE_SCHEMA_VERSION: u32 = 1;
/// 当前目录布局协议标识，与数值 schema 分开校验。
pub const DURABLE_LAYOUT_VERSION: &str = "eva.durable.v1";
/// Writer generation 文件的磁盘格式。
const WRITER_GENERATION_FORMAT: &str = "eva.writer-generation.v1";
/// Runtime owner 文件的磁盘格式。
const RUNTIME_OWNER_FORMAT: &str = "eva.runtime-owner.v1";
/// Stable daemon lock anchor marker. The anchor is never replaced or removed.
const RUNTIME_LEASE_ANCHOR_FORMAT: &str = "eva.daemon-lock-anchor.v1";
/// Versioned daemon lease record stored separately from the lock anchor.
const RUNTIME_LEASE_FORMAT: &str = "eva.daemon-lease.v1";
/// 同一进程内临时文件和 owner token 的冲突消除计数器。
static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Default daemon lease lifetime. Callers should renew substantially before this deadline.
pub const DEFAULT_RUNTIME_LEASE_TTL_MS: u128 = 30_000;

/// 长期 writer ownership 的单调 fencing generation；零表示没有 durable owner。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct WriterGeneration(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Durable backend 打开模式，决定是否允许布局初始化和获取 runtime writer ownership。
pub enum DurableBackendMode {
    /// 可创建/迁移布局，并允许调用方显式获取 runtime writer ownership。
    ReadWrite,
    /// 只验证既有布局，不创建根目录或锁文件。
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 打开文件系统 durable backend 的输入选项。
pub struct DurableBackendOptions {
    /// Backend 根目录。
    pub root: PathBuf,
    /// 读写或只读模式。
    pub mode: DurableBackendMode,
    /// Manifest 缺失时是否允许初始化；只读构造器固定为 false。
    pub create_if_missing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// `backend.manifest` 的版本与子目录名称磁盘契约。
pub struct DurableBackendManifest {
    /// 数据结构 schema 版本。
    pub schema_version: u32,
    /// 目录布局协议版本。
    pub layout_version: String,
    /// Event 子树的单段目录名。
    pub event_dir: String,
    /// Runtime state 子树目录名。
    pub state_dir: String,
    /// Task snapshot 子树目录名。
    pub task_dir: String,
    /// Audit record 子树目录名。
    pub audit_dir: String,
    /// Artifact 子树目录名。
    pub artifact_dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Manifest 在某个根目录下解析出的全部绝对/连接路径。
pub struct DurableBackendLayout {
    /// Backend 根目录。
    pub root: PathBuf,
    /// Manifest 文件路径。
    pub manifest_path: PathBuf,
    /// 布局初始化/迁移期间使用的固定 OS lock anchor 路径。
    pub migration_lock_path: PathBuf,
    /// 长期 runtime writer ownership 的锁文件路径。
    pub runtime_owner_path: PathBuf,
    /// 最近一次成功取得 ownership 的诊断记录路径；活性仍以 OS lock 为准。
    pub runtime_owner_record_path: PathBuf,
    /// 跨 writer 生命周期持久化的单调 generation 路径。
    pub writer_generation_path: PathBuf,
    /// Event 子树路径。
    pub event_dir: PathBuf,
    /// Runtime state 子树路径。
    pub state_dir: PathBuf,
    /// Task 子树路径。
    pub task_dir: PathBuf,
    /// Audit 子树路径。
    pub audit_dir: PathBuf,
    /// Artifact 子树路径。
    pub artifact_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Backend 验证后的可观察摘要。
pub struct DurableBackendReport {
    /// 已验证 schema 版本。
    pub schema_version: u32,
    /// 已验证 layout 版本。
    pub layout_version: String,
    /// `read_write`、`read_only` 或 `in_memory`。
    pub mode: String,
    /// 报告生成时当前句柄是否仍持有 migration lock；成功打开后固定为 false。
    pub migration_locked: bool,
    /// Backend 根路径文本。
    pub root: String,
    /// Event 目录路径文本。
    pub event_dir: String,
    /// State 目录路径文本。
    pub state_dir: String,
    /// Task 目录路径文本。
    pub task_dir: String,
    /// Audit 目录路径文本。
    pub audit_dir: String,
    /// Artifact 目录路径文本。
    pub artifact_dir: String,
}

/// Durable backend 实现必须暴露 manifest 并验证自身版本与布局。
pub trait DurableBackend {
    /// 返回加载或内置的 manifest。
    fn manifest(&self) -> &DurableBackendManifest;
    /// 验证版本/目录并返回不修改状态的报告。
    fn verify(&self) -> Result<DurableBackendReport, EvaError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 测试使用的无 I/O backend，仅保留当前 manifest 契约。
pub struct InMemoryDurableBackend {
    /// 当前版本 manifest。
    manifest: DurableBackendManifest,
}

#[derive(Debug)]
/// 文件系统 backend 句柄；migration lock 只在 `open` 内部短暂持有。
pub struct FileSystemDurableBackend {
    /// 从实际 manifest 解析的目录布局。
    layout: DurableBackendLayout,
    /// 已读取并验证的 manifest。
    manifest: DurableBackendManifest,
    /// 打开模式。
    mode: DurableBackendMode,
}

/// 可克隆的长期 writer ownership；最后一个 clone Drop 时释放 OS lock，固定锚点保留。
#[derive(Debug, Clone)]
pub struct DurableWriterGuard {
    inner: Arc<DurableWriterGuardInner>,
}

/// Persistent lifecycle state of a daemon lease record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableRuntimeLeaseState {
    /// The owner may mutate runtime state while it holds the OS lock anchor.
    Active,
    /// A cleanly released lease may be claimed immediately.
    Released,
}

/// Stable daemon owner identity used for an explicitly confirmed failed-start reclaim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableRuntimeLeaseIdentity {
    pid: u32,
    process_start_token: String,
    generation: WriterGeneration,
}

/// Strictly versioned daemon lease metadata stored outside the fixed lock anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableRuntimeLeaseRecord {
    state: DurableRuntimeLeaseState,
    pid: u32,
    process_start_token: String,
    generation: WriterGeneration,
    heartbeat_at_ms: u128,
    expires_at_ms: u128,
}

/// A point-in-time daemon lease observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableRuntimeLeaseProbe {
    record: Option<DurableRuntimeLeaseRecord>,
    owner_live: bool,
    expired: bool,
}

/// Owns both the fixed daemon OS-lock anchor and the durable runtime writer generation.
#[derive(Debug)]
pub struct DurableRuntimeLeaseGuard {
    anchor_path: PathBuf,
    lease_path: PathBuf,
    record: DurableRuntimeLeaseRecord,
    ttl_ms: u128,
    writer: DurableWriterGuard,
    _anchor_file: File,
}

#[derive(Debug)]
struct DurableWriterGuardInner {
    root: PathBuf,
    owner_path: PathBuf,
    owner_record_path: PathBuf,
    generation_path: PathBuf,
    owner_token: String,
    generation: WriterGeneration,
    _lock_file: File,
    process_lock: Mutex<()>,
}

#[cfg(unix)]
impl Drop for DurableWriterGuardInner {
    fn drop(&mut self) {
        // flock is tied to the open-file description, which a forked child or
        // duplicated descriptor can keep alive after this File is closed.
        // Drop cannot report an unlock error, so release best-effort before
        // the File field performs its normal close.
        let _ = self._lock_file.unlock();
    }
}

#[derive(Debug)]
/// migration lock 的 RAII 所有者；Unix 会先显式解锁，再关闭文件句柄。
struct MigrationLockGuard {
    /// 固定锁锚点；guard Drop 时释放 advisory lock。
    _lock_file: File,
    /// 锁文件路径，仅用于诊断。
    _path: PathBuf,
}

#[cfg(unix)]
impl Drop for MigrationLockGuard {
    fn drop(&mut self) {
        // A forked child or duplicated descriptor can keep the shared open-file
        // description alive, so explicitly release flock before closing this File.
        let _ = self._lock_file.unlock();
    }
}

/// 持有稳定 lock anchor 的进程级记录写锁。
#[derive(Debug)]
pub(crate) struct RecordWriteLock {
    _lock_file: File,
}

#[derive(Debug)]
struct PendingFileCleanup {
    path: PathBuf,
    armed: bool,
}

impl DurableBackendMode {
    /// 返回稳定模式文本供 manifest 报告和 CLI 使用。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadWrite => "read_write",
            Self::ReadOnly => "read_only",
        }
    }

    /// 判断模式是否禁止初始化、建目录和获取 migration lock。
    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::ReadOnly)
    }
}

impl WriterGeneration {
    /// 没有 durable writer 的兼容/内存路径使用零 generation。
    pub const ZERO: Self = Self(0);

    /// 返回下一 generation；溢出时拒绝重新使用旧 fencing 值。
    fn checked_next(self) -> Result<Self, EvaError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or_else(|| EvaError::conflict("durable writer generation exhausted"))
    }
}

impl DurableRuntimeLeaseState {
    /// Stable storage and reporting value for the lease state.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Released => "released",
        }
    }

    fn from_storage(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "released" => Some(Self::Released),
            _ => None,
        }
    }
}

impl DurableRuntimeLeaseIdentity {
    /// Builds the exact identity reported by a child whose process exit was independently observed.
    pub fn new(
        pid: u32,
        process_start_token: impl Into<String>,
        generation: WriterGeneration,
    ) -> Result<Self, EvaError> {
        let process_start_token = process_start_token.into();
        if pid == 0 {
            return Err(EvaError::invalid_argument(
                "daemon lease identity pid must be positive",
            ));
        }
        validate_process_start_token(&process_start_token)?;
        if generation == WriterGeneration::ZERO {
            return Err(EvaError::invalid_argument(
                "daemon lease identity generation must be positive",
            ));
        }
        Ok(Self {
            pid,
            process_start_token,
            generation,
        })
    }

    /// Expected OS process identifier.
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    /// Expected opaque process-incarnation token.
    pub fn process_start_token(&self) -> &str {
        &self.process_start_token
    }

    /// Expected durable writer generation.
    pub const fn generation(&self) -> WriterGeneration {
        self.generation
    }
}

fn validate_process_start_token(process_start_token: &str) -> Result<(), EvaError> {
    if process_start_token.is_empty()
        || process_start_token.contains('\n')
        || process_start_token.contains('\r')
    {
        return Err(EvaError::invalid_argument(
            "daemon lease identity process start token is invalid",
        ));
    }
    Ok(())
}

impl DurableRuntimeLeaseRecord {
    /// Current persisted lifecycle state.
    pub const fn state(&self) -> DurableRuntimeLeaseState {
        self.state
    }

    /// OS process identifier projected into the lease.
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    /// Opaque owner-incarnation token used with the generation to prevent PID reuse mistakes.
    pub fn process_start_token(&self) -> &str {
        &self.process_start_token
    }

    /// Durable writer generation fenced to this lease.
    pub const fn generation(&self) -> WriterGeneration {
        self.generation
    }

    /// Last successfully persisted heartbeat timestamp.
    pub const fn heartbeat_at_ms(&self) -> u128 {
        self.heartbeat_at_ms
    }

    /// Earliest timestamp at which a dead active owner may be reclaimed.
    pub const fn expires_at_ms(&self) -> u128 {
        self.expires_at_ms
    }

    /// Returns the PID/token/generation identity used for a failed-start reclaim decision.
    pub fn identity(&self) -> DurableRuntimeLeaseIdentity {
        DurableRuntimeLeaseIdentity {
            pid: self.pid,
            process_start_token: self.process_start_token.clone(),
            generation: self.generation,
        }
    }

    /// Whether this active lease is expired at the supplied wall-clock timestamp.
    pub const fn is_expired_at(&self, now_ms: u128) -> bool {
        matches!(self.state, DurableRuntimeLeaseState::Active) && self.expires_at_ms <= now_ms
    }

    fn active(
        writer: &DurableWriterGuard,
        process_start_token: &str,
        now_ms: u128,
        ttl_ms: u128,
    ) -> Result<Self, EvaError> {
        validate_process_start_token(process_start_token)?;
        let expires_at_ms = lease_expiry(now_ms, ttl_ms)?;
        Ok(Self {
            state: DurableRuntimeLeaseState::Active,
            pid: std::process::id(),
            process_start_token: process_start_token.to_owned(),
            generation: writer.generation(),
            heartbeat_at_ms: now_ms,
            expires_at_ms,
        })
    }

    fn to_storage(&self) -> String {
        format!(
            "format={RUNTIME_LEASE_FORMAT}\nstate={}\npid={}\nprocess_start_token={}\ngeneration={}\nheartbeat_at_ms={}\nexpires_at_ms={}\n",
            self.state.as_str(),
            self.pid,
            self.process_start_token,
            self.generation.0,
            self.heartbeat_at_ms,
            self.expires_at_ms
        )
    }

    fn from_storage(data: &str) -> Result<Self, EvaError> {
        let fields = parse_metadata(data, "durable runtime lease")?;
        if fields.len() != 7 {
            return Err(EvaError::conflict(
                "durable runtime lease has unexpected fields",
            ));
        }
        if fields.get("format").map(String::as_str) != Some(RUNTIME_LEASE_FORMAT) {
            return Err(EvaError::conflict(
                "durable runtime lease format is unsupported",
            ));
        }
        let state = fields
            .get("state")
            .and_then(|value| DurableRuntimeLeaseState::from_storage(value))
            .ok_or_else(|| EvaError::conflict("durable runtime lease state is invalid"))?;
        let pid = parse_runtime_lease_field::<u32>(&fields, "pid")?;
        if pid == 0 {
            return Err(EvaError::conflict(
                "durable runtime lease pid must be positive",
            ));
        }
        let process_start_token = fields
            .get("process_start_token")
            .filter(|value| !value.is_empty())
            .cloned()
            .ok_or_else(|| {
                EvaError::conflict("durable runtime lease process start token is invalid")
            })?;
        let generation = WriterGeneration(parse_runtime_lease_field::<u64>(&fields, "generation")?);
        if generation == WriterGeneration::ZERO {
            return Err(EvaError::conflict(
                "durable runtime lease generation must be positive",
            ));
        }
        let heartbeat_at_ms = parse_runtime_lease_field::<u128>(&fields, "heartbeat_at_ms")?;
        let expires_at_ms = parse_runtime_lease_field::<u128>(&fields, "expires_at_ms")?;
        let valid_time_range = match state {
            DurableRuntimeLeaseState::Active => expires_at_ms > heartbeat_at_ms,
            DurableRuntimeLeaseState::Released => expires_at_ms == heartbeat_at_ms,
        };
        if !valid_time_range {
            return Err(EvaError::conflict(
                "durable runtime lease time range is invalid",
            ));
        }
        Ok(Self {
            state,
            pid,
            process_start_token,
            generation,
            heartbeat_at_ms,
            expires_at_ms,
        })
    }
}

impl DurableRuntimeLeaseProbe {
    /// Parsed lease record, if the anchor exists and a record has been published.
    pub fn record(&self) -> Option<&DurableRuntimeLeaseRecord> {
        self.record.as_ref()
    }

    /// Whether another process currently holds the fixed OS-lock anchor.
    pub const fn owner_live(&self) -> bool {
        self.owner_live
    }

    /// Whether the observed active record is past its expiry timestamp.
    pub const fn expired(&self) -> bool {
        self.expired
    }
}

impl DurableBackendOptions {
    /// 构造默认允许初始化的读写选项。
    pub fn read_write(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            mode: DurableBackendMode::ReadWrite,
            create_if_missing: true,
        }
    }

    /// 构造严格要求既有布局的只读选项。
    pub fn read_only(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            mode: DurableBackendMode::ReadOnly,
            create_if_missing: false,
        }
    }

    /// 对读写模式关闭自动初始化，用于只允许打开已配置 backend 的调用方。
    pub fn without_create_if_missing(mut self) -> Self {
        self.create_if_missing = false;
        self
    }
}

impl DurableBackendManifest {
    /// 创建当前 schema/layout 的规范目录清单。
    pub fn current() -> Self {
        Self {
            schema_version: CURRENT_DURABLE_SCHEMA_VERSION,
            layout_version: DURABLE_LAYOUT_VERSION.to_owned(),
            event_dir: "events".to_owned(),
            state_dir: "state".to_owned(),
            task_dir: "tasks".to_owned(),
            audit_dir: "audit".to_owned(),
            artifact_dir: "artifacts".to_owned(),
        }
    }

    /// 序列化为固定字段的 `key=value` manifest，并保留结尾换行。
    /// 该格式没有转义层，因此目录名在解析时必须是安全单段。
    pub fn to_storage(&self) -> String {
        format!(
            "schema_version={}\nlayout_version={}\nevent_dir={}\nstate_dir={}\ntask_dir={}\naudit_dir={}\nartifact_dir={}\n",
            self.schema_version,
            self.layout_version,
            self.event_dir,
            self.state_dir,
            self.task_dir,
            self.audit_dir,
            self.artifact_dir
        )
    }

    /// 严格解析 manifest 并验证版本。
    /// 未知字段、缺字段、不安全目录段和版本不匹配均返回 Conflict；当前没有自动迁移路径，
    /// 因此不能宽松接受未来 schema 后继续写入。
    pub fn from_storage(data: &str) -> Result<Self, EvaError> {
        let mut schema_version = None;
        let mut layout_version = None;
        let mut event_dir = None;
        let mut state_dir = None;
        let mut task_dir = None;
        let mut audit_dir = None;
        let mut artifact_dir = None;

        for line in data.lines().filter(|line| !line.trim().is_empty()) {
            let Some((key, value)) = line.split_once('=') else {
                return Err(EvaError::conflict("durable backend manifest is invalid"));
            };
            match key {
                "schema_version" => {
                    schema_version = Some(value.parse::<u32>().map_err(|_| {
                        EvaError::conflict("durable backend schema version is invalid")
                    })?);
                }
                "layout_version" => layout_version = Some(value.to_owned()),
                "event_dir" => event_dir = Some(validate_layout_segment("event_dir", value)?),
                "state_dir" => state_dir = Some(validate_layout_segment("state_dir", value)?),
                "task_dir" => task_dir = Some(validate_layout_segment("task_dir", value)?),
                "audit_dir" => audit_dir = Some(validate_layout_segment("audit_dir", value)?),
                "artifact_dir" => {
                    artifact_dir = Some(validate_layout_segment("artifact_dir", value)?)
                }
                _ => {
                    return Err(
                        EvaError::conflict("durable backend manifest has unknown field")
                            .with_context("field", key),
                    );
                }
            }
        }

        let manifest = Self {
            schema_version: schema_version.ok_or_else(|| {
                EvaError::conflict("durable backend manifest missing schema_version")
            })?,
            layout_version: layout_version.ok_or_else(|| {
                EvaError::conflict("durable backend manifest missing layout_version")
            })?,
            event_dir: event_dir
                .ok_or_else(|| EvaError::conflict("durable backend manifest missing event_dir"))?,
            state_dir: state_dir
                .ok_or_else(|| EvaError::conflict("durable backend manifest missing state_dir"))?,
            task_dir: task_dir
                .ok_or_else(|| EvaError::conflict("durable backend manifest missing task_dir"))?,
            audit_dir: audit_dir
                .ok_or_else(|| EvaError::conflict("durable backend manifest missing audit_dir"))?,
            artifact_dir: artifact_dir.ok_or_else(|| {
                EvaError::conflict("durable backend manifest missing artifact_dir")
            })?,
        };
        manifest.verify_version()?;
        Ok(manifest)
    }

    /// 同时要求数值 schema 和 layout 标识匹配当前实现，并在冲突中报告 expected/actual。
    pub fn verify_version(&self) -> Result<(), EvaError> {
        if self.schema_version != CURRENT_DURABLE_SCHEMA_VERSION {
            return Err(
                EvaError::conflict("durable backend schema version mismatch")
                    .with_context("expected", CURRENT_DURABLE_SCHEMA_VERSION.to_string())
                    .with_context("actual", self.schema_version.to_string()),
            );
        }
        if self.layout_version != DURABLE_LAYOUT_VERSION {
            return Err(
                EvaError::conflict("durable backend layout version mismatch")
                    .with_context("expected", DURABLE_LAYOUT_VERSION)
                    .with_context("actual", &self.layout_version),
            );
        }
        Ok(())
    }
}

impl DurableBackendLayout {
    /// 将经过校验的 manifest 目录段连接到指定根目录。
    /// 目录段不能含分隔符，因此连接结果不能逃逸 root。
    pub fn from_manifest(root: impl Into<PathBuf>, manifest: &DurableBackendManifest) -> Self {
        let root = root.into();
        Self {
            manifest_path: root.join("backend.manifest"),
            migration_lock_path: root.join("migration.lock"),
            runtime_owner_path: root.join("runtime.writer.lock"),
            runtime_owner_record_path: root.join("runtime.writer.owner"),
            writer_generation_path: root.join("runtime.writer.generation"),
            event_dir: root.join(&manifest.event_dir),
            state_dir: root.join(&manifest.state_dir),
            task_dir: root.join(&manifest.task_dir),
            audit_dir: root.join(&manifest.audit_dir),
            artifact_dir: root.join(&manifest.artifact_dir),
            root,
        }
    }

    /// 使用当前规范 manifest 构造布局，主要供初始化和测试使用。
    pub fn current(root: impl Into<PathBuf>) -> Self {
        Self::from_manifest(root, &DurableBackendManifest::current())
    }
}

impl InMemoryDurableBackend {
    /// 创建携带当前 manifest 的内存 backend。
    pub fn new() -> Self {
        Self {
            manifest: DurableBackendManifest::current(),
        }
    }
}

impl Default for InMemoryDurableBackend {
    /// 默认值等同于 `new`，始终使用当前 manifest。
    fn default() -> Self {
        Self::new()
    }
}

impl DurableBackend for InMemoryDurableBackend {
    /// 返回内置 manifest。
    fn manifest(&self) -> &DurableBackendManifest {
        &self.manifest
    }

    /// 验证 manifest 版本并返回无锁、无真实路径的内存报告。
    fn verify(&self) -> Result<DurableBackendReport, EvaError> {
        self.manifest.verify_version()?;
        Ok(DurableBackendReport {
            schema_version: self.manifest.schema_version,
            layout_version: self.manifest.layout_version.clone(),
            mode: "in_memory".to_owned(),
            migration_locked: false,
            root: "in_memory".to_owned(),
            event_dir: self.manifest.event_dir.clone(),
            state_dir: self.manifest.state_dir.clone(),
            task_dir: self.manifest.task_dir.clone(),
            audit_dir: self.manifest.audit_dir.clone(),
            artifact_dir: self.manifest.artifact_dir.clone(),
        })
    }
}

impl FileSystemDurableBackend {
    /// 打开或初始化文件系统 backend。
    ///
    /// 只读模式从不创建 root/manifest/目录或锁。读写模式只在布局初始化/修复期间持有
    /// migration lock，manifest 通过已同步的同目录临时文件原子替换；成功返回的 backend
    /// 不再持有 migration lock。业务写入必须另行获取长期 runtime writer ownership。
    pub fn open(options: DurableBackendOptions) -> Result<Self, EvaError> {
        let current_manifest = DurableBackendManifest::current();
        let current_layout = DurableBackendLayout::from_manifest(&options.root, &current_manifest);

        if !current_layout.manifest_path.exists() {
            if options.mode.is_read_only() || !options.create_if_missing {
                return Err(
                    EvaError::not_found("durable backend manifest does not exist")
                        .with_context("path", current_layout.manifest_path.display().to_string()),
                );
            }
            fs::create_dir_all(&current_layout.root).map_err(|error| {
                EvaError::internal("failed to create durable backend root")
                    .with_context("path", current_layout.root.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
        }

        let migration_lock = if options.mode.is_read_only() {
            None
        } else {
            Some(acquire_migration_lock(&current_layout)?)
        };

        if !current_layout.manifest_path.exists() {
            if options.mode.is_read_only() || !options.create_if_missing {
                return Err(
                    EvaError::not_found("durable backend manifest does not exist")
                        .with_context("path", current_layout.manifest_path.display().to_string()),
                );
            }
            create_layout_dirs(&current_layout)?;
            atomic_write(
                &current_layout.manifest_path,
                current_manifest.to_storage().as_bytes(),
            )
            .map_err(|error| {
                EvaError::internal("failed to write durable backend manifest")
                    .with_context("path", current_layout.manifest_path.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
        }

        let manifest = read_manifest(&current_layout.manifest_path)?;
        let layout = DurableBackendLayout::from_manifest(&options.root, &manifest);
        if !options.mode.is_read_only() {
            create_layout_dirs(&layout)?;
        }
        let backend = Self {
            layout,
            manifest,
            mode: options.mode,
        };
        backend.verify()?;
        drop(migration_lock);
        Ok(backend)
    }

    /// 返回从实际 manifest 解析的布局。
    pub fn layout(&self) -> &DurableBackendLayout {
        &self.layout
    }

    /// 返回当前句柄的访问模式。
    pub fn mode(&self) -> DurableBackendMode {
        self.mode
    }

    /// 显式获取长期 runtime writer ownership，并原子分配新的 fencing generation。
    ///
    /// `open(ReadWrite)` 本身不会获取该 ownership，避免同一 daemon 的只读/迁移打开与业务
    /// writer 自冲突。返回的 guard 可克隆并传给多个 store；最后一个 clone Drop 时 OS 锁释放。
    pub fn acquire_runtime_writer(&self) -> Result<DurableWriterGuard, EvaError> {
        if self.mode.is_read_only() {
            return Err(EvaError::conflict(
                "read-only durable backend cannot acquire runtime writer ownership",
            )
            .with_context("root", self.layout.root.display().to_string()));
        }
        DurableWriterGuard::acquire(&self.layout)
    }
}

impl DurableWriterGuard {
    fn acquire(layout: &DurableBackendLayout) -> Result<Self, EvaError> {
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&layout.runtime_owner_path)
            .map_err(|error| {
                EvaError::internal("failed to open durable runtime writer lock")
                    .with_context("path", layout.runtime_owner_path.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
        lock_file.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => {
                EvaError::conflict("durable runtime writer is already owned")
                    .with_context("path", layout.runtime_owner_path.display().to_string())
            }
            TryLockError::Error(error) => {
                EvaError::internal("failed to acquire durable runtime writer lock")
                    .with_context("path", layout.runtime_owner_path.display().to_string())
                    .with_context("io_error", error.to_string())
            }
        })?;
        lock_file.sync_all().map_err(|error| {
            writer_owner_io_error(
                &layout.runtime_owner_path,
                "failed to sync runtime writer lock anchor",
                error,
            )
        })?;
        sync_parent_directory(&layout.root).map_err(|error| {
            writer_owner_io_error(
                &layout.root,
                "failed to sync runtime writer lock directory",
                error,
            )
        })?;

        let current_generation = read_writer_generation(&layout.writer_generation_path)?;
        if current_generation == WriterGeneration::ZERO && layout.runtime_owner_record_path.exists()
        {
            return Err(EvaError::conflict(
                "durable writer generation is missing after ownership was initialized",
            )
            .with_context(
                "generation_path",
                layout.writer_generation_path.display().to_string(),
            )
            .with_context(
                "owner_path",
                layout.runtime_owner_record_path.display().to_string(),
            ));
        }
        let generation = current_generation.checked_next()?;
        atomic_write(
            &layout.writer_generation_path,
            format!(
                "format={WRITER_GENERATION_FORMAT}\ngeneration={}\n",
                generation.0
            )
            .as_bytes(),
        )
        .map_err(|error| {
            EvaError::internal("failed to persist durable writer generation")
                .with_context("path", layout.writer_generation_path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;

        let owner_token = unique_token("writer", generation.0);
        atomic_write(
            &layout.runtime_owner_record_path,
            format!(
                "format={RUNTIME_OWNER_FORMAT}\nowner_token={owner_token}\ngeneration={}\npid={}\n",
                generation.0,
                std::process::id()
            )
            .as_bytes(),
        )
        .map_err(|error| {
            writer_owner_io_error(
                &layout.runtime_owner_record_path,
                "failed to persist runtime writer metadata",
                error,
            )
        })?;

        Ok(Self {
            inner: Arc::new(DurableWriterGuardInner {
                root: layout.root.clone(),
                owner_path: layout.runtime_owner_path.clone(),
                owner_record_path: layout.runtime_owner_record_path.clone(),
                generation_path: layout.writer_generation_path.clone(),
                owner_token,
                generation,
                _lock_file: lock_file,
                process_lock: Mutex::new(()),
            }),
        })
    }

    /// 返回该 owner 获得的单调 fencing generation。
    pub fn generation(&self) -> WriterGeneration {
        self.inner.generation
    }

    /// 返回 ownership 所属 durable backend 根。
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// 返回用于诊断同一 generation owner 的唯一 token。
    pub fn owner_token(&self) -> &str {
        &self.inner.owner_token
    }

    /// 重新读取持久 generation，拒绝已被后继 owner fence 的旧 guard。
    pub fn verify_current(&self) -> Result<(), EvaError> {
        let current = read_writer_generation(&self.inner.generation_path)?;
        if current != self.inner.generation {
            return Err(
                EvaError::conflict("durable runtime writer generation is stale")
                    .with_context("path", self.inner.owner_path.display().to_string())
                    .with_context("expected", self.inner.generation.0.to_string())
                    .with_context("actual", current.0.to_string()),
            );
        }
        let fields = read_metadata_file(
            &self.inner.owner_record_path,
            "durable runtime writer owner",
        )?;
        let owner_generation = fields
            .get("generation")
            .and_then(|value| value.parse::<u64>().ok());
        let owner_pid = fields
            .get("pid")
            .and_then(|value| value.parse::<u32>().ok());
        if fields.len() != 4
            || fields.get("format").map(String::as_str) != Some(RUNTIME_OWNER_FORMAT)
            || fields.get("owner_token").map(String::as_str)
                != Some(self.inner.owner_token.as_str())
            || owner_generation != Some(self.inner.generation.0)
            || owner_pid != Some(std::process::id())
        {
            return Err(EvaError::conflict(
                "durable runtime writer owner record is stale or invalid",
            )
            .with_context("path", self.inner.owner_record_path.display().to_string())
            .with_context("expected_generation", self.inner.generation.0.to_string()));
        }
        Ok(())
    }

    /// 串行化同一 owner 的 store 写入，并在进入提交区前执行 generation fencing。
    pub(crate) fn with_write_lock<T>(
        &self,
        operation: impl FnOnce(WriterGeneration) -> Result<T, EvaError>,
    ) -> Result<T, EvaError> {
        let _guard = self.inner.process_lock.lock().map_err(|_| {
            EvaError::internal("durable runtime writer process lock is poisoned")
                .with_context("path", self.inner.owner_path.display().to_string())
        })?;
        self.verify_current()?;
        operation(self.inner.generation)
    }
}

impl DurableRuntimeLeaseGuard {
    /// Claims a daemon lease while holding the fixed anchor and a new durable writer generation.
    ///
    /// A live anchor is never stolen, even if its record is expired. Once the anchor is available,
    /// an active record must also be expired before takeover; corrupt and legacy records fail closed.
    pub fn acquire(
        backend: &FileSystemDurableBackend,
        anchor_path: impl AsRef<Path>,
        lease_path: impl AsRef<Path>,
        now_ms: u128,
        ttl_ms: u128,
    ) -> Result<Self, EvaError> {
        Self::acquire_inner(
            backend,
            anchor_path.as_ref(),
            lease_path.as_ref(),
            now_ms,
            ttl_ms,
            None,
            None,
        )
    }

    /// Claims a daemon lease with a launcher-issued process incarnation token.
    ///
    /// Background launchers use this token to bind a child handle to a lease even if the child is
    /// killed before it can publish its claimed startup frame.
    pub fn acquire_with_process_start_token(
        backend: &FileSystemDurableBackend,
        anchor_path: impl AsRef<Path>,
        lease_path: impl AsRef<Path>,
        process_start_token: &str,
        now_ms: u128,
        ttl_ms: u128,
    ) -> Result<Self, EvaError> {
        validate_process_start_token(process_start_token)?;
        Self::acquire_inner(
            backend,
            anchor_path.as_ref(),
            lease_path.as_ref(),
            now_ms,
            ttl_ms,
            Some(process_start_token),
            None,
        )
    }

    /// Reclaims one exact active lease after the caller has independently observed child exit.
    ///
    /// Unlike normal acquisition, an exact dead failed-start identity may be reclaimed before its
    /// expiry. The fixed anchor must be unlocked and the current active record must match all of
    /// PID, process-start token, and writer generation; every other state fails closed.
    pub fn reclaim_failed_start(
        backend: &FileSystemDurableBackend,
        anchor_path: impl AsRef<Path>,
        lease_path: impl AsRef<Path>,
        expected: &DurableRuntimeLeaseIdentity,
        now_ms: u128,
        ttl_ms: u128,
    ) -> Result<Self, EvaError> {
        Self::acquire_inner(
            backend,
            anchor_path.as_ref(),
            lease_path.as_ref(),
            now_ms,
            ttl_ms,
            None,
            Some(expected),
        )
    }

    fn acquire_inner(
        backend: &FileSystemDurableBackend,
        anchor_path: &Path,
        lease_path: &Path,
        now_ms: u128,
        ttl_ms: u128,
        process_start_token: Option<&str>,
        failed_start_identity: Option<&DurableRuntimeLeaseIdentity>,
    ) -> Result<Self, EvaError> {
        let anchor_path = anchor_path.to_path_buf();
        let lease_path = lease_path.to_path_buf();
        validate_runtime_lease_paths(&anchor_path, &lease_path)?;
        lease_expiry(now_ms, ttl_ms)?;

        let anchor_existed = path_exists(&anchor_path, "daemon lock anchor")?;
        let lease_existed = path_exists(&lease_path, "daemon lease record")?;
        if !anchor_existed && lease_existed {
            return Err(EvaError::conflict(
                "daemon lease record exists without its fixed lock anchor",
            )
            .with_context("anchor_path", anchor_path.display().to_string())
            .with_context("lease_path", lease_path.display().to_string()));
        }
        if failed_start_identity.is_some() && !anchor_existed {
            return Err(EvaError::conflict(
                "failed-start reclaim requires an existing daemon lock anchor",
            )
            .with_context("anchor_path", anchor_path.display().to_string()));
        }

        ensure_parent_directory(&anchor_path, "daemon lock anchor")?;
        ensure_parent_directory(&lease_path, "daemon lease record")?;
        let (anchor_file, anchor_created) = acquire_runtime_lease_anchor(&anchor_path)?;
        if failed_start_identity.is_some() && anchor_created {
            return Err(EvaError::conflict(
                "daemon lock anchor changed during failed-start reclaim",
            )
            .with_context("anchor_path", anchor_path.display().to_string()));
        }
        if anchor_created && path_exists(&lease_path, "daemon lease record")? {
            return Err(EvaError::conflict(
                "daemon lease record appeared while its lock anchor was missing",
            )
            .with_context("anchor_path", anchor_path.display().to_string())
            .with_context("lease_path", lease_path.display().to_string()));
        }

        let previous = read_runtime_lease_record(&lease_path)?;
        match failed_start_identity {
            Some(expected) => {
                let record = previous.as_ref().ok_or_else(|| {
                    EvaError::conflict(
                        "failed-start reclaim requires an existing active daemon lease",
                    )
                    .with_context("lease_path", lease_path.display().to_string())
                })?;
                if record.state != DurableRuntimeLeaseState::Active {
                    return Err(EvaError::conflict(
                        "failed-start reclaim requires an active daemon lease",
                    )
                    .with_context("lease_path", lease_path.display().to_string())
                    .with_context("actual_state", record.state.as_str()));
                }
                let actual = record.identity();
                if actual != *expected {
                    return Err(EvaError::conflict(
                        "failed-start daemon lease identity does not match",
                    )
                    .with_context("lease_path", lease_path.display().to_string())
                    .with_context("expected_pid", expected.pid.to_string())
                    .with_context("actual_pid", actual.pid.to_string())
                    .with_context("expected_generation", expected.generation.0.to_string())
                    .with_context("actual_generation", actual.generation.0.to_string())
                    .with_context(
                        "process_start_token_matches",
                        (expected.process_start_token == actual.process_start_token).to_string(),
                    ));
                }
            }
            None => {
                if let Some(record) = previous.as_ref().filter(|record| {
                    record.state == DurableRuntimeLeaseState::Active
                        && !record.is_expired_at(now_ms)
                }) {
                    return Err(EvaError::conflict(
                        "daemon lease owner is dead but its lease has not expired",
                    )
                    .with_context("lease_path", lease_path.display().to_string())
                    .with_context("pid", record.pid.to_string())
                    .with_context("generation", record.generation.0.to_string())
                    .with_context("expires_at_ms", record.expires_at_ms.to_string()));
                }
            }
        }

        let persisted_generation = read_writer_generation(&backend.layout.writer_generation_path)?;
        if let Some(record) = previous.as_ref() {
            if record.generation > persisted_generation {
                return Err(EvaError::conflict(
                    "daemon lease generation is ahead of durable writer generation",
                )
                .with_context("lease_path", lease_path.display().to_string())
                .with_context("lease_generation", record.generation.0.to_string())
                .with_context("writer_generation", persisted_generation.0.to_string()));
            }
        }

        let writer = backend.acquire_runtime_writer()?;
        if let Some(record) = previous.as_ref() {
            if writer.generation() <= record.generation {
                return Err(EvaError::conflict(
                    "daemon lease takeover did not advance the writer generation",
                )
                .with_context("lease_path", lease_path.display().to_string())
                .with_context("previous_generation", record.generation.0.to_string())
                .with_context("new_generation", writer.generation().0.to_string()));
            }
        }

        let record = DurableRuntimeLeaseRecord::active(
            &writer,
            process_start_token.unwrap_or_else(|| writer.owner_token()),
            now_ms,
            ttl_ms,
        )?;
        write_runtime_lease_record(&lease_path, &record)?;
        Ok(Self {
            anchor_path,
            lease_path,
            record,
            ttl_ms,
            writer,
            _anchor_file: anchor_file,
        })
    }

    /// Current in-memory copy of the last lease record successfully written by this guard.
    pub fn record(&self) -> &DurableRuntimeLeaseRecord {
        &self.record
    }

    /// Clone the fenced writer for stores that must share this daemon's generation.
    pub fn writer(&self) -> DurableWriterGuard {
        self.writer.clone()
    }

    /// Stable OS-lock anchor path held for this guard's lifetime.
    pub fn anchor_path(&self) -> &Path {
        &self.anchor_path
    }

    /// Atomically replaced lease record path associated with the anchor.
    pub fn lease_path(&self) -> &Path {
        &self.lease_path
    }

    /// Atomically extends heartbeat and expiry after verifying both writer and lease identity.
    pub fn renew_at(&mut self, now_ms: u128) -> Result<&DurableRuntimeLeaseRecord, EvaError> {
        if self.record.state != DurableRuntimeLeaseState::Active {
            return Err(
                EvaError::conflict("released daemon lease cannot be renewed")
                    .with_context("lease_path", self.lease_path.display().to_string()),
            );
        }
        self.verify_current_record()?;
        let heartbeat_at_ms = self.record.heartbeat_at_ms.max(now_ms);
        let expires_at_ms = lease_expiry(heartbeat_at_ms, self.ttl_ms)?;
        let next = DurableRuntimeLeaseRecord {
            heartbeat_at_ms,
            expires_at_ms,
            ..self.record.clone()
        };
        write_runtime_lease_record(&self.lease_path, &next)?;
        self.record = next;
        Ok(&self.record)
    }

    /// Atomically marks a matching lease released so the next owner need not wait for expiry.
    pub fn release_at(&mut self, now_ms: u128) -> Result<&DurableRuntimeLeaseRecord, EvaError> {
        if self.record.state == DurableRuntimeLeaseState::Released {
            return Ok(&self.record);
        }
        self.verify_current_record()?;
        let heartbeat_at_ms = self.record.heartbeat_at_ms.max(now_ms);
        let next = DurableRuntimeLeaseRecord {
            state: DurableRuntimeLeaseState::Released,
            heartbeat_at_ms,
            expires_at_ms: heartbeat_at_ms,
            ..self.record.clone()
        };
        write_runtime_lease_record(&self.lease_path, &next)?;
        self.record = next;
        Ok(&self.record)
    }

    fn verify_current_record(&self) -> Result<(), EvaError> {
        self.writer.verify_current()?;
        let current = read_runtime_lease_record(&self.lease_path)?.ok_or_else(|| {
            EvaError::conflict("daemon lease record disappeared while owned")
                .with_context("lease_path", self.lease_path.display().to_string())
        })?;
        if current != self.record {
            return Err(
                EvaError::conflict("daemon lease record was replaced by another owner")
                    .with_context("lease_path", self.lease_path.display().to_string())
                    .with_context("expected_generation", self.record.generation.0.to_string())
                    .with_context("actual_generation", current.generation.0.to_string()),
            );
        }
        Ok(())
    }
}

impl Drop for DurableRuntimeLeaseGuard {
    fn drop(&mut self) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(self.record.heartbeat_at_ms);
        let _ = self.release_at(now_ms);
    }
}

/// Inspects a daemon lease without modifying the fixed anchor or lease record.
pub fn probe_runtime_lease(
    anchor_path: impl AsRef<Path>,
    lease_path: impl AsRef<Path>,
    now_ms: u128,
) -> Result<DurableRuntimeLeaseProbe, EvaError> {
    let anchor_path = anchor_path.as_ref();
    let lease_path = lease_path.as_ref();
    validate_runtime_lease_paths(anchor_path, lease_path)?;
    let anchor_exists = path_exists(anchor_path, "daemon lock anchor")?;
    let lease_exists = path_exists(lease_path, "daemon lease record")?;
    if !anchor_exists {
        if lease_exists {
            return Err(EvaError::conflict(
                "daemon lease record exists without its fixed lock anchor",
            )
            .with_context("anchor_path", anchor_path.display().to_string())
            .with_context("lease_path", lease_path.display().to_string()));
        }
        return Ok(DurableRuntimeLeaseProbe {
            record: None,
            owner_live: false,
            expired: false,
        });
    }

    let mut anchor_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(anchor_path)
        .map_err(|error| {
            runtime_lease_io_error(anchor_path, "failed to open daemon lock anchor", error)
        })?;
    let owner_live = match anchor_file.try_lock() {
        Ok(()) => false,
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(error)) => {
            return Err(runtime_lease_io_error(
                anchor_path,
                "failed to inspect daemon lock anchor",
                error,
            ))
        }
    };
    if !owner_live {
        validate_runtime_lease_anchor(&mut anchor_file, anchor_path)?;
    }
    let record = read_runtime_lease_record(lease_path)?;
    let expired = record
        .as_ref()
        .is_some_and(|record| record.is_expired_at(now_ms));
    Ok(DurableRuntimeLeaseProbe {
        record,
        owner_live,
        expired,
    })
}

impl PartialEq for DurableWriterGuard {
    fn eq(&self, other: &Self) -> bool {
        self.inner.root == other.inner.root
            && self.inner.generation == other.inner.generation
            && self.inner.owner_token == other.inner.owner_token
    }
}

impl Eq for DurableWriterGuard {}

impl DurableBackend for FileSystemDurableBackend {
    /// 返回已验证 manifest。
    fn manifest(&self) -> &DurableBackendManifest {
        &self.manifest
    }

    /// 验证版本与所有必需子目录存在，并报告本句柄是否持锁。
    /// 即使只读打开也要求完整布局，避免消费者在缺目录时得到部分可用 backend。
    fn verify(&self) -> Result<DurableBackendReport, EvaError> {
        self.manifest.verify_version()?;
        for (name, path) in [
            ("event_dir", &self.layout.event_dir),
            ("state_dir", &self.layout.state_dir),
            ("task_dir", &self.layout.task_dir),
            ("audit_dir", &self.layout.audit_dir),
            ("artifact_dir", &self.layout.artifact_dir),
        ] {
            if !path.is_dir() {
                return Err(
                    EvaError::not_found("durable backend layout directory is missing")
                        .with_context("directory", name)
                        .with_context("path", path.display().to_string()),
                );
            }
        }
        Ok(DurableBackendReport {
            schema_version: self.manifest.schema_version,
            layout_version: self.manifest.layout_version.clone(),
            mode: self.mode.as_str().to_owned(),
            migration_locked: false,
            root: self.layout.root.display().to_string(),
            event_dir: self.layout.event_dir.display().to_string(),
            state_dir: self.layout.state_dir.display().to_string(),
            task_dir: self.layout.task_dir.display().to_string(),
            audit_dir: self.layout.audit_dir.display().to_string(),
            artifact_dir: self.layout.artifact_dir.display().to_string(),
        })
    }
}

/// 幂等创建当前 manifest 声明的全部数据目录；任一 I/O 失败携带具体路径。
fn create_layout_dirs(layout: &DurableBackendLayout) -> Result<(), EvaError> {
    for path in [
        &layout.event_dir,
        &layout.state_dir,
        &layout.task_dir,
        &layout.audit_dir,
        &layout.artifact_dir,
    ] {
        fs::create_dir_all(path).map_err(|error| {
            EvaError::internal("failed to create durable backend directory")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
    }
    Ok(())
}

/// 在固定锚点上争用 OS advisory lock；进程崩溃时由内核释放，不依赖删除锁文件。
fn acquire_migration_lock(layout: &DurableBackendLayout) -> Result<MigrationLockGuard, EvaError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&layout.migration_lock_path)
        .map_err(|error| {
            EvaError::internal("failed to open durable backend migration lock")
                .with_context("path", layout.migration_lock_path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
    file.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => {
            EvaError::conflict("durable backend migration is already in progress")
                .with_context("path", layout.migration_lock_path.display().to_string())
        }
        TryLockError::Error(error) => {
            EvaError::internal("failed to acquire durable backend migration lock")
                .with_context("path", layout.migration_lock_path.display().to_string())
                .with_context("io_error", error.to_string())
        }
    })?;
    file.sync_all().map_err(|error| {
        EvaError::internal("failed to sync durable backend migration lock")
            .with_context("path", layout.migration_lock_path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    sync_parent_directory(&layout.root).map_err(|error| {
        EvaError::internal("failed to sync durable backend migration lock directory")
            .with_context("path", layout.root.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    Ok(MigrationLockGuard {
        _lock_file: file,
        _path: layout.migration_lock_path.clone(),
    })
}

/// 探测 migration lock 是否正被另一个进程持有；固定 lock 文件存在本身不表示 busy。
pub fn migration_lock_is_held(layout: &DurableBackendLayout) -> Result<bool, EvaError> {
    if !layout.migration_lock_path.exists() {
        return Ok(false);
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&layout.migration_lock_path)
        .map_err(|error| {
            EvaError::internal("failed to inspect durable backend migration lock")
                .with_context("path", layout.migration_lock_path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(EvaError::internal(
            "failed to inspect durable backend migration lock",
        )
        .with_context("path", layout.migration_lock_path.display().to_string())
        .with_context("io_error", error.to_string())),
    }
}

/// 在固定文件上获取自动随进程/句柄释放的 OS advisory lock。
pub(crate) fn acquire_record_write_lock(path: &Path) -> Result<RecordWriteLock, EvaError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            EvaError::internal("failed to create durable record lock directory")
                .with_context("path", parent.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| {
            EvaError::internal("failed to open durable record lock")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
    file.lock().map_err(|error| {
        EvaError::internal("failed to acquire durable record lock")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    Ok(RecordWriteLock { _lock_file: file })
}

fn read_writer_generation(path: &Path) -> Result<WriterGeneration, EvaError> {
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(WriterGeneration::ZERO),
        Err(error) => {
            return Err(
                EvaError::internal("failed to read durable writer generation")
                    .with_context("path", path.display().to_string())
                    .with_context("io_error", error.to_string()),
            )
        }
    };
    let fields = parse_metadata(&data, "durable writer generation")
        .map_err(|error| error.with_context("path", path.display().to_string()))?;
    if fields.len() != 2 {
        return Err(
            EvaError::conflict("durable writer generation has unexpected fields")
                .with_context("path", path.display().to_string()),
        );
    }
    if fields.get("format").map(String::as_str) != Some(WRITER_GENERATION_FORMAT) {
        return Err(
            EvaError::conflict("durable writer generation format is unsupported")
                .with_context("path", path.display().to_string()),
        );
    }
    let generation = fields
        .get("generation")
        .ok_or_else(|| EvaError::conflict("durable writer generation is incomplete"))?
        .parse::<u64>()
        .map_err(|_| {
            EvaError::conflict("durable writer generation is invalid")
                .with_context("path", path.display().to_string())
        })?;
    if generation == 0 {
        return Err(
            EvaError::conflict("persisted durable writer generation must be positive")
                .with_context("path", path.display().to_string()),
        );
    }
    Ok(WriterGeneration(generation))
}

fn validate_runtime_lease_paths(anchor_path: &Path, lease_path: &Path) -> Result<(), EvaError> {
    if anchor_path.as_os_str().is_empty() || lease_path.as_os_str().is_empty() {
        return Err(EvaError::invalid_argument(
            "daemon lease paths must not be empty",
        ));
    }
    if anchor_path == lease_path {
        return Err(EvaError::invalid_argument(
            "daemon lock anchor and lease record must use different paths",
        )
        .with_context("path", anchor_path.display().to_string()));
    }
    Ok(())
}

fn ensure_parent_directory(path: &Path, label: &'static str) -> Result<(), EvaError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            EvaError::invalid_argument(format!("{label} path must have a parent directory"))
                .with_context("path", path.display().to_string())
        })?;
    fs::create_dir_all(parent).map_err(|error| {
        runtime_lease_io_error(parent, "failed to create daemon lease directory", error)
    })
}

fn path_exists(path: &Path, label: &'static str) -> Result<bool, EvaError> {
    path.try_exists().map_err(|error| {
        EvaError::internal(format!("failed to inspect {label}"))
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })
}

fn acquire_runtime_lease_anchor(path: &Path) -> Result<(File, bool), EvaError> {
    let (mut file, created) = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => (file, false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            publish_runtime_lease_anchor(path)?
        }
        Err(error) => {
            return Err(runtime_lease_io_error(
                path,
                "failed to open daemon lock anchor",
                error,
            ))
        }
    };

    match runtime_lease_anchor_marker_matches(&mut file) {
        Ok(true) => {}
        Ok(false) => return Err(invalid_runtime_lease_anchor(path)),
        // Windows may deny reads while a live byte-range lock is held. `try_lock` below is
        // the liveness authority; if it succeeds, the mandatory post-lock read surfaces the
        // actual I/O or format error.
        Err(_) => {}
    }

    file.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => EvaError::conflict("daemon lease is owned by a live process")
            .with_context("anchor_path", path.display().to_string()),
        TryLockError::Error(error) => {
            runtime_lease_io_error(path, "failed to acquire daemon lock anchor", error)
        }
    })?;
    validate_runtime_lease_anchor(&mut file, path)?;
    Ok((file, created))
}

fn publish_runtime_lease_anchor(path: &Path) -> Result<(File, bool), EvaError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            EvaError::invalid_argument("daemon lock anchor path must have a parent directory")
                .with_context("path", path.display().to_string())
        })?;
    let file_name = path.file_name().ok_or_else(|| {
        EvaError::invalid_argument("daemon lock anchor path must have a file name")
            .with_context("path", path.display().to_string())
    })?;
    let (temp_path, mut temp_file) = loop {
        let mut temp_name = OsString::from(".");
        temp_name.push(file_name);
        temp_name.push(format!(".init-{}", unique_token("anchor", 0)));
        let candidate = parent.join(temp_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => break (candidate, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(runtime_lease_io_error(
                    &candidate,
                    "failed to create daemon lock anchor staging file",
                    error,
                ))
            }
        }
    };
    let mut cleanup = PendingFileCleanup {
        path: temp_path.clone(),
        armed: true,
    };
    temp_file
        .write_all(format!("format={RUNTIME_LEASE_ANCHOR_FORMAT}\n").as_bytes())
        .and_then(|()| temp_file.flush())
        .and_then(|()| temp_file.sync_all())
        .map_err(|error| {
            runtime_lease_io_error(
                &temp_path,
                "failed to initialize daemon lock anchor staging file",
                error,
            )
        })?;
    drop(temp_file);

    let created = match fs::hard_link(&temp_path, path) {
        Ok(()) => {
            sync_parent_directory(parent).map_err(|error| {
                runtime_lease_io_error(parent, "failed to sync daemon lock anchor directory", error)
            })?;
            true
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
        Err(error) => {
            return Err(runtime_lease_io_error(
                path,
                "failed to publish daemon lock anchor",
                error,
            ))
        }
    };
    fs::remove_file(&temp_path).map_err(|error| {
        runtime_lease_io_error(
            &temp_path,
            "failed to remove daemon lock anchor staging link",
            error,
        )
    })?;
    cleanup.armed = false;
    sync_parent_directory(parent).map_err(|error| {
        runtime_lease_io_error(parent, "failed to sync daemon lock anchor directory", error)
    })?;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| {
            runtime_lease_io_error(path, "failed to open published daemon lock anchor", error)
        })?;
    Ok((file, created))
}

fn validate_runtime_lease_anchor(file: &mut File, path: &Path) -> Result<(), EvaError> {
    if runtime_lease_anchor_marker_matches(file)
        .map_err(|error| runtime_lease_io_error(path, "failed to read daemon lock anchor", error))?
    {
        return Ok(());
    }
    Err(invalid_runtime_lease_anchor(path))
}

fn runtime_lease_anchor_marker_matches(file: &mut File) -> io::Result<bool> {
    file.seek(SeekFrom::Start(0))?;
    let mut data = String::new();
    file.read_to_string(&mut data)?;
    Ok(data == format!("format={RUNTIME_LEASE_ANCHOR_FORMAT}\n"))
}

fn invalid_runtime_lease_anchor(path: &Path) -> EvaError {
    EvaError::conflict("daemon lock anchor format is corrupt or unsupported")
        .with_context("path", path.display().to_string())
}

fn read_runtime_lease_record(path: &Path) -> Result<Option<DurableRuntimeLeaseRecord>, EvaError> {
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(EvaError::conflict("daemon lease record cannot be read")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string()))
        }
    };
    DurableRuntimeLeaseRecord::from_storage(&data)
        .map(Some)
        .map_err(|error| error.with_context("path", path.display().to_string()))
}

fn write_runtime_lease_record(
    path: &Path,
    record: &DurableRuntimeLeaseRecord,
) -> Result<(), EvaError> {
    atomic_write(path, record.to_storage().as_bytes()).map_err(|error| {
        runtime_lease_io_error(path, "failed to persist daemon lease record", error)
    })
}

fn parse_runtime_lease_field<T>(
    fields: &BTreeMap<String, String>,
    field: &'static str,
) -> Result<T, EvaError>
where
    T: std::str::FromStr,
{
    fields
        .get(field)
        .ok_or_else(|| {
            EvaError::conflict("durable runtime lease is incomplete").with_context("field", field)
        })?
        .parse::<T>()
        .map_err(|_| {
            EvaError::conflict("durable runtime lease field is invalid")
                .with_context("field", field)
        })
}

fn lease_expiry(now_ms: u128, ttl_ms: u128) -> Result<u128, EvaError> {
    if ttl_ms == 0 {
        return Err(EvaError::invalid_argument(
            "daemon lease ttl must be positive",
        ));
    }
    now_ms
        .checked_add(ttl_ms)
        .ok_or_else(|| EvaError::invalid_argument("daemon lease expiry overflows u128"))
}

fn runtime_lease_io_error(path: &Path, message: &'static str, error: io::Error) -> EvaError {
    EvaError::internal(message)
        .with_context("path", path.display().to_string())
        .with_context("io_error", error.to_string())
}

fn parse_metadata(data: &str, label: &'static str) -> Result<BTreeMap<String, String>, EvaError> {
    let mut fields = BTreeMap::new();
    for line in data.lines().filter(|line| !line.is_empty()) {
        let Some((key, value)) = line.split_once('=') else {
            return Err(EvaError::conflict(format!("{label} metadata is invalid")));
        };
        if key.is_empty() || fields.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(EvaError::conflict(format!(
                "{label} metadata contains a duplicate or empty field"
            ))
            .with_context("field", key));
        }
    }
    Ok(fields)
}

fn read_metadata_file(
    path: &Path,
    label: &'static str,
) -> Result<BTreeMap<String, String>, EvaError> {
    let data = fs::read_to_string(path).map_err(|error| {
        EvaError::conflict(format!("{label} metadata cannot be read"))
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    parse_metadata(&data, label)
        .map_err(|error| error.with_context("path", path.display().to_string()))
}

fn writer_owner_io_error(path: &Path, message: &'static str, error: io::Error) -> EvaError {
    EvaError::internal(message)
        .with_context("path", path.display().to_string())
        .with_context("io_error", error.to_string())
}

fn unique_token(prefix: &str, generation: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let counter = UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{prefix}-{}-{generation}-{now}-{counter}",
        std::process::id()
    )
}

/// 同目录 create-new temp -> write/flush/sync -> 原子替换 -> 目录同步。
pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    atomic_write_with_replace(path, data, replace_file_atomically)
}

fn atomic_write_with_replace(
    path: &Path,
    data: &[u8],
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let (temp_path, mut file) = loop {
        let mut temp_name = OsString::from(".");
        temp_name.push(file_name);
        temp_name.push(format!(
            ".tmp-{}-{}",
            std::process::id(),
            UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let candidate = parent.join(temp_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => break (candidate, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let mut cleanup = PendingFileCleanup {
        path: temp_path.clone(),
        armed: true,
    };
    file.write_all(data)?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    replace(&temp_path, path)?;
    cleanup.armed = false;
    sync_parent_directory(parent)
}

#[cfg(windows)]
fn replace_file_atomically(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    replace_windows_file_with_retry(
        || {
            // SAFETY: both buffers are NUL-terminated UTF-16 and remain alive for the call.
            let result = unsafe {
                MoveFileExW(
                    source.as_ptr(),
                    target.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            };
            if result == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        },
        std::thread::sleep,
    )
}

#[cfg(windows)]
const WINDOWS_REPLACE_MAX_ATTEMPTS: usize = 8;

#[cfg(windows)]
/// Retry only errors that can be caused by a short-lived non-delete-share handle from a scanner.
fn replace_windows_file_with_retry(
    mut replace: impl FnMut() -> io::Result<()>,
    mut wait: impl FnMut(std::time::Duration),
) -> io::Result<()> {
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;

    let mut delay_ms = 1_u64;
    for attempt in 0..WINDOWS_REPLACE_MAX_ATTEMPTS {
        match replace() {
            Ok(()) => return Ok(()),
            Err(error)
                if attempt + 1 < WINDOWS_REPLACE_MAX_ATTEMPTS
                    && matches!(
                        error.raw_os_error(),
                        Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
                    ) =>
            {
                wait(std::time::Duration::from_millis(delay_ms));
                delay_ms = delay_ms.saturating_mul(2);
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded Windows replace retry loop must return")
}

#[cfg(unix)]
fn replace_file_atomically(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)
}

#[cfg(not(any(unix, windows)))]
fn replace_file_atomically(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> io::Result<()> {
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> io::Result<()> {
    Ok(())
}

impl Drop for PendingFileCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// 读取并严格解析 manifest，在缺失/I/O/格式错误上附加文件路径。
fn read_manifest(path: &Path) -> Result<DurableBackendManifest, EvaError> {
    let data = fs::read_to_string(path).map_err(|error| {
        EvaError::not_found("durable backend manifest does not exist")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    DurableBackendManifest::from_storage(&data)
        .map_err(|error| error.with_context("path", path.display().to_string()))
}

/// 校验 manifest 目录名是非空、无边界空白且不含路径分隔符的单段。
/// 拒绝 `.`/`..`，保证 `from_manifest` 连接结果始终停留在 backend root 下。
fn validate_layout_segment(field: &'static str, value: &str) -> Result<String, EvaError> {
    if value.trim().is_empty()
        || value.trim() != value
        || value.contains('/')
        || value.contains('\\')
        || value == "."
        || value == ".."
    {
        return Err(
            EvaError::conflict("durable backend layout segment is invalid")
                .with_context("field", field)
                .with_context("value", value),
        );
    }
    Ok(value.to_owned())
}

#[cfg(test)]
/// Backend 初始化、只读语义、版本兼容和 migration lock 生命周期回归测试。
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    #[test]
    /// 验证首次读写打开原子创建布局，并在返回前释放 migration lock。
    fn filesystem_backend_creates_layout_and_manifest() {
        let root = test_root("create-layout");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let report = backend.verify().unwrap();

        assert_eq!(report.schema_version, CURRENT_DURABLE_SCHEMA_VERSION);
        assert_eq!(report.mode, "read_write");
        assert!(!report.migration_locked);
        assert!(!migration_lock_is_held(backend.layout()).unwrap());
        assert!(backend.layout().manifest_path.exists());
        assert!(backend.layout().migration_lock_path.exists());
        assert!(backend.layout().event_dir.is_dir());
        assert!(backend.layout().artifact_dir.is_dir());
    }

    #[test]
    /// 验证既有布局可只读打开且不会创建 migration lock。
    fn read_only_backend_requires_existing_layout_without_locking() {
        let root = test_root("read-only");
        drop(
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap(),
        );

        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_only(root.path())).unwrap();
        let report = backend.verify().unwrap();

        assert_eq!(backend.mode(), DurableBackendMode::ReadOnly);
        assert_eq!(report.mode, "read_only");
        assert!(!report.migration_locked);
        assert!(backend.layout().migration_lock_path.exists());
        assert!(!migration_lock_is_held(backend.layout()).unwrap());
    }

    #[test]
    /// 验证只读打开缺失 root 返回 NotFound 且不产生目录副作用。
    fn read_only_backend_does_not_create_missing_root() {
        let root = test_root("read-only-missing");
        let missing = root.path().join("missing");

        let error =
            FileSystemDurableBackend::open(DurableBackendOptions::read_only(&missing)).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::NotFound);
        assert!(!missing.exists());
    }

    #[test]
    /// 验证未知 schema 版本被拒绝，防止旧代码写入未来布局。
    fn schema_version_mismatch_is_rejected() {
        let root = test_root("version-mismatch");
        drop(
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap(),
        );
        let manifest_path = root.path().join("backend.manifest");
        let data = DurableBackendManifest {
            schema_version: CURRENT_DURABLE_SCHEMA_VERSION + 1,
            ..DurableBackendManifest::current()
        }
        .to_storage();
        fs::write(manifest_path, data).unwrap();

        let error = FileSystemDurableBackend::open(DurableBackendOptions::read_only(root.path()))
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    /// 验证读写 open 在版本校验失败时通过 guard 释放已获取的锁。
    fn failed_read_write_open_releases_migration_lock() {
        let root = test_root("failed-open-releases-lock");
        drop(
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap(),
        );
        let manifest_path = root.path().join("backend.manifest");
        let data = DurableBackendManifest {
            schema_version: CURRENT_DURABLE_SCHEMA_VERSION + 1,
            ..DurableBackendManifest::current()
        }
        .to_storage();
        fs::write(manifest_path, data).unwrap();

        let error = FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        let layout = DurableBackendLayout::current(root.path());
        assert!(!migration_lock_is_held(&layout).unwrap());
    }

    #[test]
    /// 验证两个 backend 可先后完成短 migration 阶段，但只有一个长期 runtime writer。
    fn runtime_writer_is_distinct_from_migration_lock() {
        let root = test_root("runtime-writer-lock");
        let first_backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let second_backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        assert!(!migration_lock_is_held(first_backend.layout()).unwrap());

        let first = first_backend.acquire_runtime_writer().unwrap();
        assert_eq!(first.generation(), WriterGeneration(1));
        let clone = first.clone();
        let error = second_backend.acquire_runtime_writer().unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);

        drop(first);
        assert_eq!(
            second_backend.acquire_runtime_writer().unwrap_err().kind(),
            eva_core::ErrorKind::Conflict
        );
        drop(clone);

        let second = second_backend.acquire_runtime_writer().unwrap();
        assert_eq!(second.generation(), WriterGeneration(2));
        assert!(second_backend.layout().runtime_owner_path.exists());
        assert!(second_backend.layout().runtime_owner_record_path.exists());
        assert!(second_backend.layout().writer_generation_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn runtime_writer_drop_unlocks_an_inherited_duplicate() {
        let root = test_root("writer-inherited-fd");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let inherited_lock = writer.inner._lock_file.try_clone().unwrap();
        let started = root.path().join("inherited-fd-started");
        let release = root.path().join("inherited-fd-release");
        let mut child = spawn_inherited_writer_fd_child(inherited_lock, &started, &release);
        wait_for_path(&started, Duration::from_secs(10));

        drop(writer);
        let successor = backend.acquire_runtime_writer().unwrap();
        assert_eq!(successor.generation(), WriterGeneration(2));

        fs::write(&release, b"release").unwrap();
        assert!(child.wait().unwrap().success());
    }

    #[test]
    /// 验证固定 migration anchor 的 OS lock 活性探测不依赖文件是否存在。
    fn migration_lock_probe_observes_guard_lifetime() {
        let root = test_root("migration-probe");
        fs::create_dir_all(root.path()).unwrap();
        let layout = DurableBackendLayout::current(root.path());
        let guard = acquire_migration_lock(&layout).unwrap();

        assert!(migration_lock_is_held(&layout).unwrap());
        drop(guard);
        assert!(!migration_lock_is_held(&layout).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn migration_lock_drop_unlocks_a_duplicated_descriptor() {
        let root = test_root("migration-duplicated-fd");
        fs::create_dir_all(root.path()).unwrap();
        let layout = DurableBackendLayout::current(root.path());
        let guard = acquire_migration_lock(&layout).unwrap();
        let duplicated_lock = guard._lock_file.try_clone().unwrap();

        drop(guard);
        assert!(!migration_lock_is_held(&layout).unwrap());

        drop(duplicated_lock);
    }

    #[test]
    /// 验证 generation 损坏和耗尽均 fail closed，且不会重置既有字节。
    fn runtime_writer_rejects_corrupt_or_exhausted_generation() {
        let root = test_root("generation-corrupt");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        fs::write(
            &backend.layout().writer_generation_path,
            "generation=broken\n",
        )
        .unwrap();
        let corrupt = fs::read(&backend.layout().writer_generation_path).unwrap();

        assert_eq!(
            backend.acquire_runtime_writer().unwrap_err().kind(),
            eva_core::ErrorKind::Conflict
        );
        assert_eq!(
            fs::read(&backend.layout().writer_generation_path).unwrap(),
            corrupt
        );

        fs::write(
            &backend.layout().writer_generation_path,
            format!(
                "format={WRITER_GENERATION_FORMAT}\ngeneration={}\n",
                u64::MAX
            ),
        )
        .unwrap();
        assert_eq!(
            backend.acquire_runtime_writer().unwrap_err().kind(),
            eva_core::ErrorKind::Conflict
        );
    }

    #[test]
    /// 已发布 owner 记录存在时，generation 丢失必须 fail closed，不能从 1 重新开始。
    fn runtime_writer_does_not_reuse_deleted_generation() {
        let root = test_root("generation-deleted");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        assert_eq!(writer.generation(), WriterGeneration(1));
        drop(writer);
        let owner_bytes = fs::read(&backend.layout().runtime_owner_record_path).unwrap();
        fs::remove_file(&backend.layout().writer_generation_path).unwrap();

        let error = backend.acquire_runtime_writer().unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert!(!backend.layout().writer_generation_path.exists());
        assert_eq!(
            fs::read(&backend.layout().runtime_owner_record_path).unwrap(),
            owner_bytes
        );
    }

    #[test]
    /// 验证 replacement 返回失败时保留完整旧值并清除已同步的同目录临时文件。
    fn atomic_write_failure_preserves_previous_record() {
        let root = test_root("atomic-write-failure");
        fs::create_dir_all(root.path()).unwrap();
        let target = root.path().join("record.state");
        atomic_write(&target, b"old-record\n").unwrap();

        let error = atomic_write_with_replace(&target, b"new-record\n", |_, _| {
            Err(io::Error::other("injected replace failure"))
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(fs::read(&target).unwrap(), b"old-record\n");
        assert!(!fs::read_dir(root.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp-")
        }));
    }

    #[cfg(windows)]
    #[test]
    fn windows_replace_retries_only_transient_interference() {
        let errors = [5, 32, 33];
        let mut attempts = 0_usize;
        let mut delays = Vec::new();

        replace_windows_file_with_retry(
            || {
                let result = errors
                    .get(attempts)
                    .copied()
                    .map(io::Error::from_raw_os_error)
                    .map_or(Ok(()), Err);
                attempts += 1;
                result
            },
            |delay| delays.push(delay),
        )
        .unwrap();

        assert_eq!(attempts, 4);
        assert_eq!(
            delays,
            [
                Duration::from_millis(1),
                Duration::from_millis(2),
                Duration::from_millis(4),
            ]
        );
        let mut permanent_attempts = 0_usize;
        let error = replace_windows_file_with_retry(
            || {
                permanent_attempts += 1;
                Err(io::Error::from_raw_os_error(5))
            },
            |_| {},
        )
        .unwrap_err();
        assert_eq!(permanent_attempts, WINDOWS_REPLACE_MAX_ATTEMPTS);
        assert_eq!(error.raw_os_error(), Some(5));

        let mut non_transient_attempts = 0_usize;
        let error = replace_windows_file_with_retry(
            || {
                non_transient_attempts += 1;
                Err(io::Error::from_raw_os_error(3))
            },
            |_| panic!("non-transient errors must not wait"),
        )
        .unwrap_err();
        assert_eq!(non_transient_attempts, 1);
        assert_eq!(error.raw_os_error(), Some(3));
    }

    #[test]
    /// 验证两个真实测试进程同时竞争时恰好一个 writer，失败者不递增 generation。
    fn two_processes_compete_for_one_runtime_writer() {
        let root = test_root("two-process-writers");
        drop(
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap(),
        );
        let start = root.path().join("start");
        let release = root.path().join("release");
        let outcome_a = root.path().join("outcome-a");
        let outcome_b = root.path().join("outcome-b");
        let mut child_a = spawn_writer_child(root.path(), &start, &release, &outcome_a);
        let mut child_b = spawn_writer_child(root.path(), &start, &release, &outcome_b);
        fs::write(&start, b"go").unwrap();

        wait_for_path(&outcome_a, Duration::from_secs(10));
        wait_for_path(&outcome_b, Duration::from_secs(10));
        let outcomes = [
            fs::read_to_string(&outcome_a).unwrap(),
            fs::read_to_string(&outcome_b).unwrap(),
        ];
        assert_eq!(
            outcomes
                .iter()
                .filter(|value| value.as_str() == "acquired")
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|value| value.as_str() == "conflict")
                .count(),
            1
        );
        let layout = DurableBackendLayout::current(root.path());
        assert_eq!(
            read_writer_generation(&layout.writer_generation_path).unwrap(),
            WriterGeneration(1)
        );

        fs::write(&release, b"release").unwrap();
        assert!(child_a.wait().unwrap().success());
        assert!(child_b.wait().unwrap().success());
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        assert_eq!(
            backend.acquire_runtime_writer().unwrap().generation(),
            WriterGeneration(2)
        );
    }

    #[test]
    fn runtime_writer_process_child() {
        let Ok(root) = std::env::var("EVA_STORAGE_WRITER_CHILD_ROOT") else {
            return;
        };
        let start = PathBuf::from(std::env::var("EVA_STORAGE_WRITER_CHILD_START").unwrap());
        let release = PathBuf::from(std::env::var("EVA_STORAGE_WRITER_CHILD_RELEASE").unwrap());
        let outcome = PathBuf::from(std::env::var("EVA_STORAGE_WRITER_CHILD_OUTCOME").unwrap());
        wait_for_path(&start, Duration::from_secs(10));
        let deadline = Instant::now() + Duration::from_secs(5);
        let backend = loop {
            match FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)) {
                Ok(backend) => break backend,
                Err(error)
                    if error.kind() == eva_core::ErrorKind::Conflict
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    fs::write(&outcome, format!("open-error:{}", error.kind().as_str())).unwrap();
                    return;
                }
            }
        };
        match backend.acquire_runtime_writer() {
            Ok(_writer) => {
                fs::write(&outcome, b"acquired").unwrap();
                wait_for_path(&release, Duration::from_secs(10));
            }
            Err(error) if error.kind() == eva_core::ErrorKind::Conflict => {
                fs::write(&outcome, b"conflict").unwrap();
            }
            Err(error) => {
                fs::write(&outcome, format!("writer-error:{}", error.kind().as_str())).unwrap();
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn inherited_writer_fd_process_child() {
        let Ok(started) = std::env::var("EVA_STORAGE_INHERITED_FD_STARTED") else {
            return;
        };
        let release = PathBuf::from(std::env::var("EVA_STORAGE_INHERITED_FD_RELEASE").unwrap());
        fs::write(started, b"started").unwrap();
        wait_for_path(&release, Duration::from_secs(10));
    }

    #[test]
    /// 验证内存 backend 仍遵守当前 manifest 且不报告锁。
    fn in_memory_backend_keeps_test_implementation_available() {
        let backend = InMemoryDurableBackend::new();
        let report = backend.verify().unwrap();

        assert_eq!(
            backend.manifest().schema_version,
            CURRENT_DURABLE_SCHEMA_VERSION
        );
        assert_eq!(report.mode, "in_memory");
        assert!(!report.migration_locked);
    }

    #[test]
    fn runtime_lease_process_child() {
        let Ok(root) = std::env::var("EVA_STORAGE_LEASE_CHILD_ROOT") else {
            return;
        };
        let anchor = PathBuf::from(std::env::var("EVA_STORAGE_LEASE_CHILD_ANCHOR").unwrap());
        let lease = PathBuf::from(std::env::var("EVA_STORAGE_LEASE_CHILD_LEASE").unwrap());
        let start = PathBuf::from(std::env::var("EVA_STORAGE_LEASE_CHILD_START").unwrap());
        let release = PathBuf::from(std::env::var("EVA_STORAGE_LEASE_CHILD_RELEASE").unwrap());
        let outcome = PathBuf::from(std::env::var("EVA_STORAGE_LEASE_CHILD_OUTCOME").unwrap());
        wait_for_path(&start, Duration::from_secs(10));
        let deadline = Instant::now() + Duration::from_secs(5);
        let backend = loop {
            match FileSystemDurableBackend::open(DurableBackendOptions::read_write(&root)) {
                Ok(backend) => break backend,
                Err(error)
                    if error.kind() == eva_core::ErrorKind::Conflict
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    fs::write(&outcome, format!("open-error:{}", error.kind().as_str())).unwrap();
                    return;
                }
            }
        };
        match DurableRuntimeLeaseGuard::acquire(&backend, &anchor, &lease, 200, 50) {
            Ok(guard) => {
                fs::write(
                    &outcome,
                    format!("acquired:{}", guard.record().generation().0),
                )
                .unwrap();
                wait_for_path(&release, Duration::from_secs(10));
            }
            Err(error) if error.kind() == eva_core::ErrorKind::Conflict => {
                fs::write(&outcome, b"conflict").unwrap();
            }
            Err(error) => {
                fs::write(&outcome, format!("lease-error:{}", error.kind().as_str())).unwrap();
            }
        }
    }

    #[test]
    fn two_processes_reclaim_one_expired_daemon_lease_once() {
        let root = test_root("two-process-daemon-lease");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let anchor = root.path().join("daemon.lock");
        let lease = root.path().join("daemon.lease");
        let dead = seed_dead_runtime_lease(&backend, &anchor, &lease, 100, 50);
        assert_eq!(dead.generation(), WriterGeneration(1));
        let start = root.path().join("lease-start");
        let release = root.path().join("lease-release");
        let outcome_a = root.path().join("lease-outcome-a");
        let outcome_b = root.path().join("lease-outcome-b");
        let mut child_a =
            spawn_runtime_lease_child(root.path(), &anchor, &lease, &start, &release, &outcome_a);
        let mut child_b =
            spawn_runtime_lease_child(root.path(), &anchor, &lease, &start, &release, &outcome_b);
        fs::write(&start, b"go").unwrap();

        wait_for_path(&outcome_a, Duration::from_secs(10));
        wait_for_path(&outcome_b, Duration::from_secs(10));
        let outcomes = [
            fs::read_to_string(&outcome_a).unwrap(),
            fs::read_to_string(&outcome_b).unwrap(),
        ];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| outcome.as_str() == "acquired:2")
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| outcome.as_str() == "conflict")
                .count(),
            1
        );
        assert_eq!(
            read_writer_generation(&backend.layout().writer_generation_path).unwrap(),
            WriterGeneration(2)
        );

        fs::write(&release, b"release").unwrap();
        assert!(child_a.wait().unwrap().success());
        assert!(child_b.wait().unwrap().success());
        let released = probe_runtime_lease(&anchor, &lease, 201).unwrap();
        assert!(!released.owner_live());
        assert_eq!(
            released.record().map(DurableRuntimeLeaseRecord::state),
            Some(DurableRuntimeLeaseState::Released)
        );

        let successor =
            DurableRuntimeLeaseGuard::acquire(&backend, &anchor, &lease, 202, 50).unwrap();
        assert_eq!(successor.record().generation(), WriterGeneration(3));
        drop(successor);
        assert!(anchor.is_file());
    }

    #[test]
    fn two_processes_initialize_one_daemon_anchor_without_stranding_empty_file() {
        let root = test_root("two-process-daemon-anchor-init");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let anchor = root.path().join("daemon.lock");
        let lease = root.path().join("daemon.lease");
        let start = root.path().join("lease-init-start");
        let release = root.path().join("lease-init-release");
        let outcome_a = root.path().join("lease-init-outcome-a");
        let outcome_b = root.path().join("lease-init-outcome-b");
        let mut child_a =
            spawn_runtime_lease_child(root.path(), &anchor, &lease, &start, &release, &outcome_a);
        let mut child_b =
            spawn_runtime_lease_child(root.path(), &anchor, &lease, &start, &release, &outcome_b);
        fs::write(&start, b"go").unwrap();

        wait_for_path(&outcome_a, Duration::from_secs(10));
        wait_for_path(&outcome_b, Duration::from_secs(10));
        let outcomes = [
            fs::read_to_string(&outcome_a).unwrap(),
            fs::read_to_string(&outcome_b).unwrap(),
        ];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| outcome.as_str() == "acquired:1")
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| outcome.as_str() == "conflict")
                .count(),
            1
        );
        assert!(anchor.is_file());
        assert_eq!(
            read_writer_generation(&backend.layout().writer_generation_path).unwrap(),
            WriterGeneration(1)
        );

        fs::write(&release, b"release").unwrap();
        assert!(child_a.wait().unwrap().success());
        assert!(child_b.wait().unwrap().success());
        let released = probe_runtime_lease(&anchor, &lease, 201).unwrap();
        assert!(!released.owner_live());
        assert_eq!(
            released.record().map(DurableRuntimeLeaseRecord::state),
            Some(DurableRuntimeLeaseState::Released)
        );
        assert_eq!(
            fs::read_to_string(&anchor).unwrap(),
            format!("format={RUNTIME_LEASE_ANCHOR_FORMAT}\n")
        );
    }

    #[test]
    fn live_expired_daemon_lease_cannot_be_stolen() {
        let root = test_root("live-expired-daemon-lease");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let anchor = root.path().join("daemon.lock");
        let lease = root.path().join("daemon.lease");
        let guard = DurableRuntimeLeaseGuard::acquire(&backend, &anchor, &lease, 100, 30)
            .expect("first lease should be acquired");

        let probe = probe_runtime_lease(&anchor, &lease, 131).unwrap();
        assert!(probe.owner_live());
        assert!(probe.expired());
        assert_eq!(probe.record(), Some(guard.record()));
        let error =
            DurableRuntimeLeaseGuard::acquire(&backend, &anchor, &lease, 131, 30).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(
            read_writer_generation(&backend.layout().writer_generation_path).unwrap(),
            WriterGeneration(1)
        );
        drop(guard);
        assert!(anchor.exists());
        assert_eq!(
            fs::read_to_string(&anchor).unwrap(),
            format!("format={RUNTIME_LEASE_ANCHOR_FORMAT}\n")
        );
    }

    #[test]
    fn dead_daemon_lease_waits_for_expiry_then_advances_generation() {
        let root = test_root("dead-daemon-lease");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let anchor = root.path().join("daemon.lock");
        let lease = root.path().join("daemon.lease");
        let dead = seed_dead_runtime_lease(&backend, &anchor, &lease, 100, 100);

        let error =
            DurableRuntimeLeaseGuard::acquire(&backend, &anchor, &lease, 199, 100).unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(
            read_writer_generation(&backend.layout().writer_generation_path).unwrap(),
            WriterGeneration(1)
        );

        let successor =
            DurableRuntimeLeaseGuard::acquire(&backend, &anchor, &lease, 200, 100).unwrap();
        assert_eq!(successor.record().generation(), WriterGeneration(2));
        assert_eq!(successor.record().pid(), dead.pid());
        assert_ne!(
            successor.record().process_start_token(),
            dead.process_start_token()
        );
        assert_eq!(successor.record().heartbeat_at_ms(), 200);
        assert_eq!(successor.record().expires_at_ms(), 300);
    }

    #[test]
    fn daemon_heartbeat_atomically_extends_only_the_matching_lease() {
        let root = test_root("daemon-heartbeat");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let anchor = root.path().join("daemon.lock");
        let lease = root.path().join("daemon.lease");
        let mut guard =
            DurableRuntimeLeaseGuard::acquire(&backend, &anchor, &lease, 100, 30).unwrap();
        let renewed = guard.renew_at(120).unwrap().clone();
        assert_eq!(renewed.heartbeat_at_ms(), 120);
        assert_eq!(renewed.expires_at_ms(), 150);
        assert_eq!(
            read_runtime_lease_record(&lease).unwrap(),
            Some(renewed.clone())
        );
        let unchanged = guard.renew_at(110).unwrap();
        assert_eq!(unchanged.heartbeat_at_ms(), 120);
        assert_eq!(unchanged.expires_at_ms(), 150);
        drop(guard);
        assert_eq!(
            fs::read_to_string(&anchor).unwrap(),
            format!("format={RUNTIME_LEASE_ANCHOR_FORMAT}\n")
        );
    }

    #[test]
    fn stale_daemon_guard_cannot_renew_or_release_successor_record() {
        let root = test_root("stale-daemon-guard");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let anchor = root.path().join("daemon.lock");
        let lease = root.path().join("daemon.lease");
        let mut guard =
            DurableRuntimeLeaseGuard::acquire(&backend, &anchor, &lease, 100, 30).unwrap();
        let successor = DurableRuntimeLeaseRecord {
            process_start_token: "successor-token".to_owned(),
            generation: WriterGeneration(2),
            heartbeat_at_ms: 200,
            expires_at_ms: 230,
            ..guard.record().clone()
        };
        write_runtime_lease_record(&lease, &successor).unwrap();
        let successor_bytes = fs::read(&lease).unwrap();

        assert_eq!(
            guard.renew_at(210).unwrap_err().kind(),
            eva_core::ErrorKind::Conflict
        );
        assert_eq!(fs::read(&lease).unwrap(), successor_bytes);
        assert_eq!(
            guard.release_at(210).unwrap_err().kind(),
            eva_core::ErrorKind::Conflict
        );
        assert_eq!(fs::read(&lease).unwrap(), successor_bytes);
        drop(guard);
        assert_eq!(fs::read(&lease).unwrap(), successor_bytes);
    }

    #[test]
    fn corrupt_or_legacy_daemon_metadata_fails_closed_before_writer_claim() {
        let root = test_root("corrupt-daemon-lease");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let anchor = root.path().join("daemon.lock");
        let lease = root.path().join("daemon.lease");
        drop(acquire_runtime_lease_anchor(&anchor).unwrap().0);
        fs::write(&lease, b"format=legacy.daemon-lock.v0\npid=123\n").unwrap();
        let lease_bytes = fs::read(&lease).unwrap();

        let error =
            DurableRuntimeLeaseGuard::acquire(&backend, &anchor, &lease, 1_000, 30).unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(fs::read(&lease).unwrap(), lease_bytes);
        assert_eq!(
            read_writer_generation(&backend.layout().writer_generation_path).unwrap(),
            WriterGeneration::ZERO
        );
        assert_eq!(
            probe_runtime_lease(&anchor, &lease, 1_000)
                .unwrap_err()
                .kind(),
            eva_core::ErrorKind::Conflict
        );

        fs::write(&anchor, b"pid=123\n").unwrap();
        fs::remove_file(&lease).unwrap();
        let anchor_bytes = fs::read(&anchor).unwrap();
        let error =
            DurableRuntimeLeaseGuard::acquire(&backend, &anchor, &lease, 1_000, 30).unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(fs::read(&anchor).unwrap(), anchor_bytes);
        assert!(!lease.exists());
    }

    #[test]
    fn daemon_lease_generation_ahead_of_writer_fails_without_burning_generations() {
        let root = test_root("daemon-lease-generation-ahead");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let anchor = root.path().join("daemon.lock");
        let lease = root.path().join("daemon.lease");
        let mut first =
            DurableRuntimeLeaseGuard::acquire(&backend, &anchor, &lease, 100, 30).unwrap();
        first.release_at(101).unwrap();
        drop(first);
        let forged = fs::read_to_string(&lease)
            .unwrap()
            .replace("generation=1\n", "generation=100\n");
        fs::write(&lease, forged.as_bytes()).unwrap();
        let generation_bytes = fs::read(&backend.layout().writer_generation_path).unwrap();
        let lease_bytes = fs::read(&lease).unwrap();

        for _ in 0..2 {
            let error =
                DurableRuntimeLeaseGuard::acquire(&backend, &anchor, &lease, 102, 30).unwrap_err();
            assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
            assert!(error.message().contains("ahead"));
            assert_eq!(
                fs::read(&backend.layout().writer_generation_path).unwrap(),
                generation_bytes
            );
            assert_eq!(fs::read(&lease).unwrap(), lease_bytes);
        }
    }

    #[test]
    fn missing_anchor_with_lease_record_fails_closed_without_creating_anchor() {
        let root = test_root("missing-daemon-anchor");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let anchor = root.path().join("daemon.lock");
        let lease = root.path().join("daemon.lease");
        fs::write(
            &lease,
            "format=eva.daemon-lease.v1\nstate=active\npid=1\nprocess_start_token=orphan\ngeneration=1\nheartbeat_at_ms=100\nexpires_at_ms=200\n",
        )
        .unwrap();
        let lease_bytes = fs::read(&lease).unwrap();

        assert_eq!(
            DurableRuntimeLeaseGuard::acquire(&backend, &anchor, &lease, 200, 30)
                .unwrap_err()
                .kind(),
            eva_core::ErrorKind::Conflict
        );
        assert!(!anchor.exists());
        assert_eq!(fs::read(&lease).unwrap(), lease_bytes);
        assert_eq!(
            probe_runtime_lease(&anchor, &lease, 200)
                .unwrap_err()
                .kind(),
            eva_core::ErrorKind::Conflict
        );
    }

    #[test]
    fn released_daemon_lease_can_be_reclaimed_immediately() {
        let root = test_root("released-daemon-lease");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let anchor = root.path().join("daemon.lock");
        let lease = root.path().join("daemon.lease");
        let mut first =
            DurableRuntimeLeaseGuard::acquire(&backend, &anchor, &lease, 100, 1_000).unwrap();
        first.release_at(101).unwrap();
        assert_eq!(first.record().state(), DurableRuntimeLeaseState::Released);
        drop(first);

        let second =
            DurableRuntimeLeaseGuard::acquire(&backend, &anchor, &lease, 102, 1_000).unwrap();
        assert_eq!(second.record().generation(), WriterGeneration(2));
        assert_eq!(second.record().state(), DurableRuntimeLeaseState::Active);
        assert!(anchor.exists());
    }

    #[test]
    fn failed_start_reclaim_allows_exact_fresh_dead_identity() {
        let root = test_root("failed-start-reclaim-exact");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let anchor = root.path().join("daemon.lock");
        let lease = root.path().join("daemon.lease");
        let previous = seed_dead_runtime_lease(&backend, &anchor, &lease, 100, 1_000);
        let expected = previous.identity();

        assert_eq!(
            DurableRuntimeLeaseGuard::acquire(&backend, &anchor, &lease, 101, 30)
                .unwrap_err()
                .kind(),
            eva_core::ErrorKind::Conflict
        );
        let successor = DurableRuntimeLeaseGuard::reclaim_failed_start(
            &backend, &anchor, &lease, &expected, 101, 30,
        )
        .unwrap();
        assert_eq!(successor.record().generation(), WriterGeneration(2));
        assert_eq!(successor.record().state(), DurableRuntimeLeaseState::Active);
        assert_eq!(successor.record().heartbeat_at_ms(), 101);
        assert_eq!(successor.record().expires_at_ms(), 131);
        assert_ne!(
            successor.record().process_start_token(),
            expected.process_start_token()
        );
    }

    #[test]
    fn failed_start_reclaim_rejects_live_released_and_mismatched_identity() {
        let root = test_root("failed-start-reclaim-rejections");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let anchor = root.path().join("daemon.lock");
        let lease = root.path().join("daemon.lease");

        let live =
            DurableRuntimeLeaseGuard::acquire(&backend, &anchor, &lease, 100, 1_000).unwrap();
        let live_identity = live.record().identity();
        assert_eq!(
            DurableRuntimeLeaseGuard::reclaim_failed_start(
                &backend,
                &anchor,
                &lease,
                &live_identity,
                101,
                30,
            )
            .unwrap_err()
            .kind(),
            eva_core::ErrorKind::Conflict
        );
        drop(live);

        let previous = seed_dead_runtime_lease(&backend, &anchor, &lease, 100, 1_000);
        assert_eq!(previous.state(), DurableRuntimeLeaseState::Active);
        assert_eq!(previous.generation(), WriterGeneration(2));
        let expected = previous.identity();
        let lease_bytes = fs::read(&lease).unwrap();
        let wrong_pid = if expected.pid() == u32::MAX {
            expected.pid() - 1
        } else {
            expected.pid() + 1
        };
        let mismatches = [
            DurableRuntimeLeaseIdentity::new(
                wrong_pid,
                expected.process_start_token(),
                expected.generation(),
            )
            .unwrap(),
            DurableRuntimeLeaseIdentity::new(
                expected.pid(),
                "different-process-incarnation",
                expected.generation(),
            )
            .unwrap(),
            DurableRuntimeLeaseIdentity::new(
                expected.pid(),
                expected.process_start_token(),
                WriterGeneration(expected.generation().0 + 1),
            )
            .unwrap(),
        ];
        for mismatch in mismatches {
            assert_eq!(
                DurableRuntimeLeaseGuard::reclaim_failed_start(
                    &backend, &anchor, &lease, &mismatch, 101, 30,
                )
                .unwrap_err()
                .kind(),
                eva_core::ErrorKind::Conflict
            );
            assert_eq!(fs::read(&lease).unwrap(), lease_bytes);
            assert_eq!(
                read_writer_generation(&backend.layout().writer_generation_path).unwrap(),
                WriterGeneration(2)
            );
        }

        let released = DurableRuntimeLeaseRecord {
            state: DurableRuntimeLeaseState::Released,
            expires_at_ms: previous.heartbeat_at_ms,
            ..previous
        };
        write_runtime_lease_record(&lease, &released).unwrap();
        let released_bytes = fs::read(&lease).unwrap();
        assert_eq!(
            DurableRuntimeLeaseGuard::reclaim_failed_start(
                &backend, &anchor, &lease, &expected, 101, 30,
            )
            .unwrap_err()
            .kind(),
            eva_core::ErrorKind::Conflict
        );
        assert_eq!(fs::read(&lease).unwrap(), released_bytes);
    }

    #[test]
    fn failed_start_reclaim_rejects_corrupt_and_missing_anchor_metadata() {
        let root = test_root("failed-start-reclaim-corrupt");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let anchor = root.path().join("daemon.lock");
        let lease = root.path().join("daemon.lease");
        drop(acquire_runtime_lease_anchor(&anchor).unwrap().0);
        fs::write(&lease, b"format=legacy.daemon.v0\npid=1\n").unwrap();
        let expected = DurableRuntimeLeaseIdentity::new(1, "child", WriterGeneration(1)).unwrap();
        let corrupt_bytes = fs::read(&lease).unwrap();
        assert_eq!(
            DurableRuntimeLeaseGuard::reclaim_failed_start(
                &backend, &anchor, &lease, &expected, 101, 30,
            )
            .unwrap_err()
            .kind(),
            eva_core::ErrorKind::Conflict
        );
        assert_eq!(fs::read(&lease).unwrap(), corrupt_bytes);
        assert_eq!(
            read_writer_generation(&backend.layout().writer_generation_path).unwrap(),
            WriterGeneration::ZERO
        );

        fs::remove_file(&anchor).unwrap();
        assert_eq!(
            DurableRuntimeLeaseGuard::reclaim_failed_start(
                &backend, &anchor, &lease, &expected, 101, 30,
            )
            .unwrap_err()
            .kind(),
            eva_core::ErrorKind::Conflict
        );
        assert!(!anchor.exists());
        assert_eq!(fs::read(&lease).unwrap(), corrupt_bytes);
    }

    #[test]
    fn failed_start_reclaim_cannot_overwrite_a_successor_identity() {
        let root = test_root("failed-start-reclaim-successor");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let anchor = root.path().join("daemon.lock");
        let lease = root.path().join("daemon.lease");
        let previous = seed_dead_runtime_lease(&backend, &anchor, &lease, 100, 1_000);
        let expected = previous.identity();
        let successor = seed_dead_runtime_lease(&backend, &anchor, &lease, 200, 1_000);
        assert_eq!(successor.generation(), WriterGeneration(2));
        let successor_bytes = fs::read(&lease).unwrap();

        assert_eq!(
            DurableRuntimeLeaseGuard::reclaim_failed_start(
                &backend, &anchor, &lease, &expected, 201, 30,
            )
            .unwrap_err()
            .kind(),
            eva_core::ErrorKind::Conflict
        );
        assert_eq!(fs::read(&lease).unwrap(), successor_bytes);
        assert_eq!(
            read_writer_generation(&backend.layout().writer_generation_path).unwrap(),
            WriterGeneration(2)
        );
    }

    fn seed_dead_runtime_lease(
        backend: &FileSystemDurableBackend,
        anchor: &Path,
        lease: &Path,
        heartbeat_at_ms: u128,
        ttl_ms: u128,
    ) -> DurableRuntimeLeaseRecord {
        let (anchor_file, _) = acquire_runtime_lease_anchor(anchor).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let record = DurableRuntimeLeaseRecord::active(
            &writer,
            writer.owner_token(),
            heartbeat_at_ms,
            ttl_ms,
        )
        .unwrap();
        write_runtime_lease_record(lease, &record).unwrap();
        drop(writer);
        drop(anchor_file);
        record
    }

    fn spawn_runtime_lease_child(
        root: &Path,
        anchor: &Path,
        lease: &Path,
        start: &Path,
        release: &Path,
        outcome: &Path,
    ) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "durable_backend::tests::runtime_lease_process_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("EVA_STORAGE_LEASE_CHILD_ROOT", root)
            .env("EVA_STORAGE_LEASE_CHILD_ANCHOR", anchor)
            .env("EVA_STORAGE_LEASE_CHILD_LEASE", lease)
            .env("EVA_STORAGE_LEASE_CHILD_START", start)
            .env("EVA_STORAGE_LEASE_CHILD_RELEASE", release)
            .env("EVA_STORAGE_LEASE_CHILD_OUTCOME", outcome)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn spawn_writer_child(root: &Path, start: &Path, release: &Path, outcome: &Path) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "durable_backend::tests::runtime_writer_process_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("EVA_STORAGE_WRITER_CHILD_ROOT", root)
            .env("EVA_STORAGE_WRITER_CHILD_START", start)
            .env("EVA_STORAGE_WRITER_CHILD_RELEASE", release)
            .env("EVA_STORAGE_WRITER_CHILD_OUTCOME", outcome)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[cfg(unix)]
    fn spawn_inherited_writer_fd_child(
        inherited_lock: File,
        started: &Path,
        release: &Path,
    ) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "durable_backend::tests::inherited_writer_fd_process_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("EVA_STORAGE_INHERITED_FD_STARTED", started)
            .env("EVA_STORAGE_INHERITED_FD_RELEASE", release)
            .stdin(Stdio::from(inherited_lock))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn wait_for_path(path: &Path, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {}",
                path.display()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// 测试临时根目录所有者。
    struct TestRoot {
        /// 唯一临时路径。
        path: PathBuf,
    }

    impl TestRoot {
        /// 返回临时路径。
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestRoot {
        /// 测试结束时尽力递归清理。
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// 用测试名、进程和纳秒时间构造并行安全路径。
    fn test_root(name: &str) -> TestRoot {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        TestRoot {
            path: std::env::temp_dir().join(format!(
                "eva-storage-durable-backend-{name}-{}-{now}",
                std::process::id()
            )),
        }
    }
}
