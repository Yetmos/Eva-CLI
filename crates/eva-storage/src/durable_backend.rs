//! Durable backend schema, layout, and migration lock baseline.

use eva_core::EvaError;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "durable backend schema, layout, and migration locks";

pub const CURRENT_DURABLE_SCHEMA_VERSION: u32 = 1;
pub const DURABLE_LAYOUT_VERSION: &str = "eva.durable.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableBackendMode {
    ReadWrite,
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableBackendOptions {
    pub root: PathBuf,
    pub mode: DurableBackendMode,
    pub create_if_missing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableBackendManifest {
    pub schema_version: u32,
    pub layout_version: String,
    pub event_dir: String,
    pub state_dir: String,
    pub task_dir: String,
    pub audit_dir: String,
    pub artifact_dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableBackendLayout {
    pub root: PathBuf,
    pub manifest_path: PathBuf,
    pub migration_lock_path: PathBuf,
    pub event_dir: PathBuf,
    pub state_dir: PathBuf,
    pub task_dir: PathBuf,
    pub audit_dir: PathBuf,
    pub artifact_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableBackendReport {
    pub schema_version: u32,
    pub layout_version: String,
    pub mode: String,
    pub migration_locked: bool,
    pub root: String,
    pub event_dir: String,
    pub state_dir: String,
    pub task_dir: String,
    pub audit_dir: String,
    pub artifact_dir: String,
}

pub trait DurableBackend {
    fn manifest(&self) -> &DurableBackendManifest;
    fn verify(&self) -> Result<DurableBackendReport, EvaError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InMemoryDurableBackend {
    manifest: DurableBackendManifest,
}

#[derive(Debug)]
pub struct FileSystemDurableBackend {
    layout: DurableBackendLayout,
    manifest: DurableBackendManifest,
    mode: DurableBackendMode,
    migration_lock: Option<MigrationLockGuard>,
}

#[derive(Debug, PartialEq, Eq)]
struct MigrationLockGuard {
    path: PathBuf,
}

impl DurableBackendMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadWrite => "read_write",
            Self::ReadOnly => "read_only",
        }
    }

    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::ReadOnly)
    }
}

impl DurableBackendOptions {
    pub fn read_write(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            mode: DurableBackendMode::ReadWrite,
            create_if_missing: true,
        }
    }

    pub fn read_only(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            mode: DurableBackendMode::ReadOnly,
            create_if_missing: false,
        }
    }

    pub fn without_create_if_missing(mut self) -> Self {
        self.create_if_missing = false;
        self
    }
}

impl DurableBackendManifest {
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

    pub fn current(root: impl Into<PathBuf>) -> Self {
        Self::from_manifest(root, &DurableBackendManifest::current())
    }
}

impl InMemoryDurableBackend {
    pub fn new() -> Self {
        Self {
            manifest: DurableBackendManifest::current(),
        }
    }
}

impl Default for InMemoryDurableBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl DurableBackend for InMemoryDurableBackend {
    fn manifest(&self) -> &DurableBackendManifest {
        &self.manifest
    }

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

    pub fn layout(&self) -> &DurableBackendLayout {
        &self.layout
    }

    pub fn mode(&self) -> DurableBackendMode {
        self.mode
    }
}

impl DurableBackend for FileSystemDurableBackend {
    fn manifest(&self) -> &DurableBackendManifest {
        &self.manifest
    }

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
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn read_manifest(path: &Path) -> Result<DurableBackendManifest, EvaError> {
    let data = fs::read_to_string(path).map_err(|error| {
        EvaError::not_found("durable backend manifest does not exist")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    DurableBackendManifest::from_storage(&data)
        .map_err(|error| error.with_context("path", path.display().to_string()))
}

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
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
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
    fn read_only_backend_does_not_create_missing_root() {
        let root = test_root("read-only-missing");
        let missing = root.path().join("missing");

        let error =
            FileSystemDurableBackend::open(DurableBackendOptions::read_only(&missing)).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::NotFound);
        assert!(!missing.exists());
    }

    #[test]
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
                "eva-storage-durable-backend-{name}-{}-{now}",
                std::process::id()
            )),
        }
    }
}
