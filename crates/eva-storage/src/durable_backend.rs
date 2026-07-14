//! Durable backend 的 schema、目录布局、兼容校验与单写者 migration lock 基线。
//! Durable backend schema, layout, and migration lock baseline.

use eva_core::EvaError;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// 本模块的架构职责：定义持久化根布局，并在读写打开期间用文件系统 CAS 锁保护迁移。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "durable backend schema, layout, and migration locks";

/// 当前可读写的 manifest schema 版本；不匹配时拒绝打开，避免无迁移器的隐式升级。
pub const CURRENT_DURABLE_SCHEMA_VERSION: u32 = 1;
/// 当前目录布局协议标识，与数值 schema 分开校验。
pub const DURABLE_LAYOUT_VERSION: &str = "eva.durable.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Durable backend 打开模式，决定是否创建目录和持有 migration lock。
pub enum DurableBackendMode {
    /// 可创建/修改布局，并在句柄生命周期内独占 migration lock。
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
    /// 读写打开期间的 migration lock 路径。
    pub migration_lock_path: PathBuf,
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
    /// 当前句柄是否持有 migration lock。
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
/// 文件系统 backend 句柄；读写模式下通过 guard 持有 migration lock。
pub struct FileSystemDurableBackend {
    /// 从实际 manifest 解析的目录布局。
    layout: DurableBackendLayout,
    /// 已读取并验证的 manifest。
    manifest: DurableBackendManifest,
    /// 打开模式。
    mode: DurableBackendMode,
    /// 读写模式的可选锁 guard；Drop 释放锁文件。
    migration_lock: Option<MigrationLockGuard>,
}

#[derive(Debug, PartialEq, Eq)]
/// migration lock 的 RAII 所有者，确保成功或失败退出作用域时都尝试释放。
struct MigrationLockGuard {
    /// 已通过 `create_new` 独占创建的锁文件路径。
    path: PathBuf,
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
    /// 只读模式从不创建 root/manifest/目录或锁。读写模式先用 `create_new` 获取 migration lock
    /// （文件系统级 compare-and-create），随后才初始化/读取 manifest 和目录；guard 在任何后续
    /// 错误返回时自动 Drop。Manifest 当前直接写最终路径，不具备临时文件 rename 的崩溃原子性，
    /// 因此重开会严格拒绝半写/损坏文件，而不会猜测修复。
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
            create_layout_dirs(&current_layout)?;
            fs::write(&current_layout.manifest_path, current_manifest.to_storage()).map_err(
                |error| {
                    EvaError::internal("failed to write durable backend manifest")
                        .with_context("path", current_layout.manifest_path.display().to_string())
                        .with_context("io_error", error.to_string())
                },
            )?;
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
            migration_lock,
        };
        backend.verify()?;
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
}

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
            migration_locked: self.migration_lock.is_some(),
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

/// 通过 `OpenOptions::create_new(true)` 原子争用单写者 migration lock。
/// AlreadyExists 映射为 Conflict；锁内容记录预期 schema/layout 供人工诊断。锁文件创建后若
/// 内容写入失败，局部 guard 尚未构造，当前实现可能留下锁文件，需要操作者确认后清理。
fn acquire_migration_lock(layout: &DurableBackendLayout) -> Result<MigrationLockGuard, EvaError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&layout.migration_lock_path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                EvaError::conflict("durable backend migration lock already exists")
                    .with_context("path", layout.migration_lock_path.display().to_string())
            } else {
                EvaError::internal("failed to create durable backend migration lock")
                    .with_context("path", layout.migration_lock_path.display().to_string())
                    .with_context("io_error", error.to_string())
            }
        })?;
    file.write_all(
        format!(
            "schema_version={}\nlayout_version={}\n",
            CURRENT_DURABLE_SCHEMA_VERSION, DURABLE_LAYOUT_VERSION
        )
        .as_bytes(),
    )
    .map_err(|error| {
        EvaError::internal("failed to write durable backend migration lock")
            .with_context("path", layout.migration_lock_path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    Ok(MigrationLockGuard {
        path: layout.migration_lock_path.clone(),
    })
}

impl Drop for MigrationLockGuard {
    /// 句柄释放或 open 后续失败展开栈时尽力删除锁；清理错误不会在 Drop 中 panic。
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
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
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    /// 验证首次读写打开创建 manifest、所有目录并持有锁。
    fn filesystem_backend_creates_layout_and_manifest() {
        let root = test_root("create-layout");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let report = backend.verify().unwrap();

        assert_eq!(report.schema_version, CURRENT_DURABLE_SCHEMA_VERSION);
        assert_eq!(report.mode, "read_write");
        assert!(report.migration_locked);
        assert!(backend.layout().manifest_path.exists());
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
        assert!(!backend.layout().migration_lock_path.exists());
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
        assert!(!root.path().join("migration.lock").exists());
    }

    #[test]
    /// 验证 create_new 锁阻止第二写者，并在首句柄 Drop 后允许重试。
    fn migration_lock_blocks_second_writer_until_drop() {
        let root = test_root("migration-lock");
        let first =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();

        let error = FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path()))
            .unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);

        drop(first);
        FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
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
