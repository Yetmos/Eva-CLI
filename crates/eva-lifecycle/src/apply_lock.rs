//! Upgrade apply lock acquisition boundary.

use eva_core::{EvaError, GenerationId};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "upgrade apply command lock model";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeApplyPlan {
    pub plan_id: String,
    pub from_generation: GenerationId,
    pub to_generation: GenerationId,
    pub from_release: String,
    pub to_release: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeApplyLock {
    pub lock_id: String,
    pub plan_id: String,
    pub owner: String,
    pub from_generation: GenerationId,
    pub to_generation: GenerationId,
    pub status: String,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeApplyReport {
    pub plan_id: String,
    pub status: String,
    pub apply_allowed: bool,
    pub lock: UpgradeApplyLock,
    pub steps: Vec<String>,
    pub risks: Vec<String>,
    pub audit: Vec<String>,
}

pub trait UpgradeApplyLockStore {
    fn acquire_lock(
        &mut self,
        plan: &UpgradeApplyPlan,
        owner: &str,
    ) -> Result<UpgradeApplyLock, EvaError>;
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryUpgradeApplyLockStore {
    locks: BTreeMap<String, UpgradeApplyLock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemUpgradeApplyLockStore {
    root: PathBuf,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UpgradeApplyCoordinator;

impl UpgradeApplyPlan {
    pub fn new(
        plan_id: impl Into<String>,
        from_generation: GenerationId,
        to_generation: GenerationId,
        from_release: impl Into<String>,
        to_release: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let plan_id = validate_token("plan_id", plan_id.into())?;
        if from_generation == to_generation {
            return Err(EvaError::invalid_argument(
                "upgrade apply plan must target a different generation",
            )
            .with_context("plan_id", &plan_id)
            .with_context("generation", from_generation.as_str()));
        }
        Ok(Self {
            plan_id,
            from_generation,
            to_generation,
            from_release: validate_release_ref("from_release", from_release.into())?,
            to_release: validate_release_ref("to_release", to_release.into())?,
        })
    }

    pub fn lock_id(&self) -> String {
        format!("upgrade-apply-{}", self.plan_id)
    }
}

impl InMemoryUpgradeApplyLockStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl FileSystemUpgradeApplyLockStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl UpgradeApplyLockStore for InMemoryUpgradeApplyLockStore {
    fn acquire_lock(
        &mut self,
        plan: &UpgradeApplyPlan,
        owner: &str,
    ) -> Result<UpgradeApplyLock, EvaError> {
        if self.locks.contains_key(&plan.plan_id) {
            return Err(lock_conflict(&plan.plan_id, None));
        }
        let lock = build_lock(plan, owner)?;
        self.locks.insert(plan.plan_id.clone(), lock.clone());
        Ok(lock)
    }
}

impl UpgradeApplyLockStore for FileSystemUpgradeApplyLockStore {
    fn acquire_lock(
        &mut self,
        plan: &UpgradeApplyPlan,
        owner: &str,
    ) -> Result<UpgradeApplyLock, EvaError> {
        let lock = build_lock(plan, owner)?;
        fs::create_dir_all(&self.root).map_err(|error| {
            EvaError::internal("failed to create upgrade apply lock store")
                .with_context("lock_store", self.root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let lock_path = lock_path(&self.root, &plan.plan_id);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    lock_conflict(&plan.plan_id, Some(&lock_path))
                } else {
                    EvaError::internal("failed to create upgrade apply lock")
                        .with_context("plan_id", &plan.plan_id)
                        .with_context("lock_path", lock_path.display().to_string())
                        .with_context("io_error", error.to_string())
                }
            })?;
        file.write_all(lock_payload(plan, &lock).as_bytes())
            .map_err(|error| {
                EvaError::internal("failed to write upgrade apply lock")
                    .with_context("plan_id", &plan.plan_id)
                    .with_context("lock_path", lock_path.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
        Ok(lock)
    }
}

impl UpgradeApplyCoordinator {
    pub fn acquire_lock<S: UpgradeApplyLockStore>(
        &self,
        store: &mut S,
        plan: &UpgradeApplyPlan,
        owner: &str,
    ) -> Result<UpgradeApplyReport, EvaError> {
        let lock = store.acquire_lock(plan, owner)?;
        Ok(UpgradeApplyReport {
            plan_id: plan.plan_id.clone(),
            status: "locked".to_owned(),
            apply_allowed: false,
            lock,
            steps: vec![
                "acquire upgrade apply lock".to_owned(),
                "verify backup evidence before destructive apply".to_owned(),
                "keep runtime mutation disabled until apply gates are complete".to_owned(),
            ],
            risks: vec![
                "upgrade apply is locked only; no runtime process is started".to_owned(),
                "destructive generation handoff still requires backup evidence and policy approval"
                    .to_owned(),
            ],
            audit: vec![
                "upgrade.apply:plan_parsed".to_owned(),
                "upgrade.apply:lock_acquired".to_owned(),
                "apply_allowed:false".to_owned(),
            ],
        })
    }
}

fn build_lock(plan: &UpgradeApplyPlan, owner: &str) -> Result<UpgradeApplyLock, EvaError> {
    let owner = validate_token("owner", owner.to_owned())?;
    Ok(UpgradeApplyLock {
        lock_id: plan.lock_id(),
        plan_id: plan.plan_id.clone(),
        owner,
        from_generation: plan.from_generation.clone(),
        to_generation: plan.to_generation.clone(),
        status: "acquired".to_owned(),
        audit: vec![
            "lock:acquired".to_owned(),
            format!("plan:{}", plan.plan_id),
            format!("from:{}", plan.from_generation.as_str()),
            format!("to:{}", plan.to_generation.as_str()),
        ],
    })
}

fn validate_token(field: &'static str, value: String) -> Result<String, EvaError> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(EvaError::invalid_argument(
            "upgrade apply token must be non-empty and trimmed",
        )
        .with_context("field", field)
        .with_context("value", value));
    }
    if value.contains("..")
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        return Err(
            EvaError::invalid_argument("upgrade apply token must be a stable slug")
                .with_context("field", field)
                .with_context("value", value),
        );
    }
    Ok(value)
}

fn validate_release_ref(field: &'static str, value: String) -> Result<String, EvaError> {
    if value.trim().is_empty()
        || value.trim() != value
        || value.contains('\n')
        || value.contains('\r')
    {
        return Err(EvaError::invalid_argument(
            "upgrade apply release ref must be non-empty and single-line",
        )
        .with_context("field", field)
        .with_context("value", value));
    }
    Ok(value)
}

fn lock_path(root: &Path, plan_id: &str) -> PathBuf {
    root.join(format!("{plan_id}.lock"))
}

fn lock_payload(plan: &UpgradeApplyPlan, lock: &UpgradeApplyLock) -> String {
    format!(
        "lock_id={}\nplan_id={}\nowner={}\nfrom_generation={}\nto_generation={}\nfrom_release={}\nto_release={}\nstatus={}\n",
        lock.lock_id,
        plan.plan_id,
        lock.owner,
        plan.from_generation.as_str(),
        plan.to_generation.as_str(),
        plan.from_release,
        plan.to_release,
        lock.status
    )
}

fn lock_conflict(plan_id: &str, lock_path: Option<&Path>) -> EvaError {
    let mut error =
        EvaError::conflict("upgrade apply lock already exists").with_context("plan_id", plan_id);
    if let Some(lock_path) = lock_path {
        error = error.with_context("lock_path", lock_path.display().to_string());
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn plan() -> UpgradeApplyPlan {
        UpgradeApplyPlan::new(
            "plan-1",
            GenerationId::parse("gen-v14").unwrap(),
            GenerationId::parse("gen-v15").unwrap(),
            "1.4.0",
            "1.5.1",
        )
        .unwrap()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("eva-lifecycle-{name}-{}-{now}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn upgrade_apply_acquires_lock_without_allowing_apply() {
        let mut store = InMemoryUpgradeApplyLockStore::new();
        let report = UpgradeApplyCoordinator
            .acquire_lock(&mut store, &plan(), "cli")
            .unwrap();

        assert_eq!(report.status, "locked");
        assert_eq!(report.lock.status, "acquired");
        assert!(!report.apply_allowed);
    }

    #[test]
    fn upgrade_apply_rejects_conflicting_lock() {
        let mut store = InMemoryUpgradeApplyLockStore::new();
        let plan = plan();

        UpgradeApplyCoordinator
            .acquire_lock(&mut store, &plan, "cli")
            .unwrap();
        let error = UpgradeApplyCoordinator
            .acquire_lock(&mut store, &plan, "cli")
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    fn filesystem_lock_persists_conflict() {
        let root = temp_dir("upgrade-lock");
        let plan = plan();
        let mut first = FileSystemUpgradeApplyLockStore::new(&root);
        let mut second = FileSystemUpgradeApplyLockStore::new(&root);

        UpgradeApplyCoordinator
            .acquire_lock(&mut first, &plan, "cli")
            .unwrap();
        let error = UpgradeApplyCoordinator
            .acquire_lock(&mut second, &plan, "cli")
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        fs::remove_dir_all(root).unwrap();
    }
}
