//! Durable backend 的 schema、目录布局、迁移互斥与长期 writer ownership。
//! Durable backend schema, layout, migration exclusion, and writer ownership.

use eva_core::EvaError;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
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
/// 同一进程内临时文件和 owner token 的冲突消除计数器。
static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(1);

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

#[derive(Debug)]
/// migration lock 的 RAII 所有者，File Drop 时由 OS 自动释放。
struct MigrationLockGuard {
    /// 固定锁锚点；文件句柄 Drop 时由 OS 自动释放 advisory lock。
    _lock_file: File,
    /// 锁文件路径，仅用于诊断。
    _path: PathBuf,
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
            || owner_pid.is_none()
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
pub(crate) fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
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
