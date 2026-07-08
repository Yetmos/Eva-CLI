//! Restore apply validation, lock, policy, and health gate boundaries.

use crate::manifest_verifier::ManifestVerifier;
use eva_core::EvaError;
use eva_policy::PolicyDecision;
use eva_storage::ArtifactRecord;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "restore apply validation, lock, policy, and health gate over durable backup artifacts";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreRestoreBackupEvidence {
    pub backup_artifact_id: String,
    pub backup_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreApplyPlan {
    pub plan_id: String,
    pub backup_artifact_id: String,
    pub backup_digest: String,
    pub pre_restore_backup: Option<PreRestoreBackupEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreApplyDryRunReport {
    pub plan_id: String,
    pub backup_artifact_key: String,
    pub expected_digest: String,
    pub actual_digest: String,
    pub pre_restore_backup_artifact_key: String,
    pub pre_restore_expected_digest: String,
    pub pre_restore_actual_digest: String,
    pub status: String,
    pub apply_allowed: bool,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreApplyLock {
    pub lock_id: String,
    pub plan_id: String,
    pub owner: String,
    pub status: String,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreApplyHealthCheck {
    pub healthy: bool,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreApplyReport {
    pub plan_id: String,
    pub status: String,
    pub apply_allowed: bool,
    pub mutation_executed: bool,
    pub lock: RestoreApplyLock,
    pub health: RestoreApplyHealthCheck,
    pub backup_artifact_key: String,
    pub pre_restore_backup_artifact_key: String,
    pub steps: Vec<String>,
    pub risks: Vec<String>,
    pub audit: Vec<String>,
}

pub trait RestoreApplyLockStore {
    fn acquire_lock(
        &mut self,
        plan: &RestoreApplyPlan,
        owner: &str,
    ) -> Result<RestoreApplyLock, EvaError>;
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryRestoreApplyLockStore {
    locks: BTreeMap<String, RestoreApplyLock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemRestoreApplyLockStore {
    root: PathBuf,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RestoreApplyValidator;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RestoreApplyCoordinator;

impl RestoreApplyPlan {
    pub fn new(
        plan_id: impl Into<String>,
        backup_artifact_id: impl Into<String>,
        backup_digest: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let plan_id = validate_token("plan_id", plan_id.into())?;
        let backup_artifact_id = validate_token("backup_artifact_id", backup_artifact_id.into())?;
        let backup_digest = backup_digest.into();
        if !backup_digest.starts_with("sha256:") {
            return Err(EvaError::invalid_argument(
                "restore apply plan backup digest must be sha256",
            )
            .with_context("plan_id", &plan_id)
            .with_context("backup_digest", backup_digest));
        }
        Ok(Self {
            plan_id,
            backup_artifact_id,
            backup_digest,
            pre_restore_backup: None,
        })
    }

    pub fn backup_artifact_key(&self) -> String {
        format!("backup/{}", self.backup_artifact_id)
    }

    pub fn lock_id(&self) -> String {
        format!("restore-apply-{}", self.plan_id)
    }

    pub fn with_pre_restore_backup(mut self, evidence: PreRestoreBackupEvidence) -> Self {
        self.pre_restore_backup = Some(evidence);
        self
    }
}

impl PreRestoreBackupEvidence {
    pub fn new(
        backup_artifact_id: impl Into<String>,
        backup_digest: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let backup_artifact_id =
            validate_token("pre_restore_backup_artifact_id", backup_artifact_id.into())?;
        let backup_digest = backup_digest.into();
        if !backup_digest.starts_with("sha256:") {
            return Err(
                EvaError::invalid_argument("pre-restore backup digest must be sha256")
                    .with_context("backup_artifact_id", &backup_artifact_id)
                    .with_context("backup_digest", backup_digest),
            );
        }
        Ok(Self {
            backup_artifact_id,
            backup_digest,
        })
    }

    pub fn backup_artifact_key(&self) -> String {
        format!("backup/{}", self.backup_artifact_id)
    }
}

impl RestoreApplyHealthCheck {
    pub fn healthy() -> Self {
        Self {
            healthy: true,
            message: "healthy".to_owned(),
        }
    }

    pub fn failed(message: impl Into<String>) -> Result<Self, EvaError> {
        let message = message.into();
        if message.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "restore apply health failure message is required",
            ));
        }
        Ok(Self {
            healthy: false,
            message,
        })
    }
}

impl InMemoryRestoreApplyLockStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl FileSystemRestoreApplyLockStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl RestoreApplyLockStore for InMemoryRestoreApplyLockStore {
    fn acquire_lock(
        &mut self,
        plan: &RestoreApplyPlan,
        owner: &str,
    ) -> Result<RestoreApplyLock, EvaError> {
        if self.locks.contains_key(&plan.plan_id) {
            return Err(restore_lock_conflict(&plan.plan_id, None));
        }
        let lock = build_restore_lock(plan, owner)?;
        self.locks.insert(plan.plan_id.clone(), lock.clone());
        Ok(lock)
    }
}

impl RestoreApplyLockStore for FileSystemRestoreApplyLockStore {
    fn acquire_lock(
        &mut self,
        plan: &RestoreApplyPlan,
        owner: &str,
    ) -> Result<RestoreApplyLock, EvaError> {
        let lock = build_restore_lock(plan, owner)?;
        fs::create_dir_all(&self.root).map_err(|error| {
            EvaError::internal("failed to create restore apply lock store")
                .with_context("lock_store", self.root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let lock_path = restore_lock_path(&self.root, &plan.plan_id);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    restore_lock_conflict(&plan.plan_id, Some(&lock_path))
                } else {
                    EvaError::internal("failed to create restore apply lock")
                        .with_context("plan_id", &plan.plan_id)
                        .with_context("lock_path", lock_path.display().to_string())
                        .with_context("io_error", error.to_string())
                }
            })?;
        file.write_all(restore_lock_payload(plan, &lock).as_bytes())
            .map_err(|error| {
                EvaError::internal("failed to write restore apply lock")
                    .with_context("plan_id", &plan.plan_id)
                    .with_context("lock_path", lock_path.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
        Ok(lock)
    }
}

impl RestoreApplyValidator {
    pub fn dry_run(
        &self,
        plan: &RestoreApplyPlan,
        artifact: &ArtifactRecord,
        pre_restore_artifact: Option<&ArtifactRecord>,
    ) -> Result<RestoreApplyDryRunReport, EvaError> {
        let expected_key = plan.backup_artifact_key();
        if artifact.key != expected_key {
            return Err(
                EvaError::conflict("restore apply backup artifact key mismatch")
                    .with_context("plan_id", &plan.plan_id)
                    .with_context("expected_artifact_key", expected_key)
                    .with_context("actual_artifact_key", &artifact.key),
            );
        }
        let pre_restore = plan.pre_restore_backup.as_ref().ok_or_else(|| {
            EvaError::invalid_argument("restore apply requires pre-restore backup evidence")
                .with_context("plan_id", &plan.plan_id)
                .with_context("required_field", "pre_restore_backup_artifact_id")
                .with_context("required_field", "pre_restore_backup_digest")
        })?;
        let pre_restore_artifact = pre_restore_artifact.ok_or_else(|| {
            EvaError::not_found("restore apply pre-restore backup artifact is missing")
                .with_context("plan_id", &plan.plan_id)
                .with_context("artifact_key", pre_restore.backup_artifact_key())
        })?;
        let expected_pre_restore_key = pre_restore.backup_artifact_key();
        if pre_restore_artifact.key != expected_pre_restore_key {
            return Err(EvaError::conflict(
                "restore apply pre-restore backup artifact key mismatch",
            )
            .with_context("plan_id", &plan.plan_id)
            .with_context("expected_artifact_key", expected_pre_restore_key)
            .with_context("actual_artifact_key", &pre_restore_artifact.key));
        }
        let verification = ManifestVerifier::verify_artifact(artifact, &plan.backup_digest)?;
        let pre_restore_verification =
            ManifestVerifier::verify_artifact(pre_restore_artifact, &pre_restore.backup_digest)?;
        Ok(RestoreApplyDryRunReport {
            plan_id: plan.plan_id.clone(),
            backup_artifact_key: artifact.key.clone(),
            expected_digest: verification.expected_digest,
            actual_digest: verification.actual_digest,
            pre_restore_backup_artifact_key: pre_restore_artifact.key.clone(),
            pre_restore_expected_digest: pre_restore_verification.expected_digest,
            pre_restore_actual_digest: pre_restore_verification.actual_digest,
            status: "dry_run_validated".to_owned(),
            apply_allowed: false,
            audit: vec![
                "restore.apply:dry_run".to_owned(),
                "backup:verified".to_owned(),
                "pre_restore_backup:verified".to_owned(),
                "apply_allowed:false".to_owned(),
            ],
        })
    }
}

impl RestoreApplyCoordinator {
    pub fn apply<S: RestoreApplyLockStore>(
        &self,
        store: &mut S,
        plan: &RestoreApplyPlan,
        dry_run: &RestoreApplyDryRunReport,
        policy_decision: &PolicyDecision,
        health: RestoreApplyHealthCheck,
        owner: &str,
    ) -> Result<RestoreApplyReport, EvaError> {
        policy_decision.ensure_allowed()?;
        let lock = store.acquire_lock(plan, owner)?;
        let mut audit = vec![
            "restore.apply:plan_parsed".to_owned(),
            "restore.apply:confirmation_matched".to_owned(),
            "restore.apply:backup_evidence_verified".to_owned(),
            "restore.apply:policy_allowed".to_owned(),
            "restore.apply:lock_acquired".to_owned(),
        ];
        audit.extend(
            policy_decision
                .audit
                .iter()
                .map(|entry| format!("policy:{entry}")),
        );
        if health.healthy {
            audit.push("restore.apply:health_check_passed".to_owned());
            audit.push("restore.apply:gated".to_owned());
            Ok(RestoreApplyReport {
                plan_id: plan.plan_id.clone(),
                status: "gated".to_owned(),
                apply_allowed: true,
                mutation_executed: false,
                lock,
                health,
                backup_artifact_key: dry_run.backup_artifact_key.clone(),
                pre_restore_backup_artifact_key: dry_run.pre_restore_backup_artifact_key.clone(),
                steps: restore_apply_steps(true),
                risks: vec![
                    "destructive restore remains bound to explicit apply gate evidence".to_owned(),
                    "release pointer mutation and supervisor handoff remain separate gates"
                        .to_owned(),
                ],
                audit,
            })
        } else {
            audit.push("restore.apply:health_check_failed".to_owned());
            audit.push("restore.apply:rollback_required".to_owned());
            Ok(RestoreApplyReport {
                plan_id: plan.plan_id.clone(),
                status: "blocked".to_owned(),
                apply_allowed: false,
                mutation_executed: false,
                lock,
                health,
                backup_artifact_key: dry_run.backup_artifact_key.clone(),
                pre_restore_backup_artifact_key: dry_run.pre_restore_backup_artifact_key.clone(),
                steps: restore_apply_steps(false),
                risks: vec![
                    "restore apply health check failed before destructive mutation".to_owned(),
                    "rollback plan must be emitted before retrying apply".to_owned(),
                ],
                audit,
            })
        }
    }
}

fn validate_token(field: &'static str, value: String) -> Result<String, EvaError> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(EvaError::invalid_argument(
            "restore apply plan token must be non-empty and trimmed",
        )
        .with_context("field", field)
        .with_context("value", value));
    }
    if value.contains('/') || value.contains('\\') || value.contains("..") {
        return Err(
            EvaError::invalid_argument("restore apply plan token must be a stable slug")
                .with_context("field", field)
                .with_context("value", value),
        );
    }
    Ok(value)
}

fn build_restore_lock(plan: &RestoreApplyPlan, owner: &str) -> Result<RestoreApplyLock, EvaError> {
    let owner = validate_token("owner", owner.to_owned())?;
    Ok(RestoreApplyLock {
        lock_id: plan.lock_id(),
        plan_id: plan.plan_id.clone(),
        owner,
        status: "acquired".to_owned(),
        audit: vec![
            "lock:acquired".to_owned(),
            format!("plan:{}", plan.plan_id),
            format!("backup:{}", plan.backup_artifact_id),
        ],
    })
}

fn restore_lock_path(root: &Path, plan_id: &str) -> PathBuf {
    root.join(format!("{plan_id}.restore.lock"))
}

fn restore_lock_payload(plan: &RestoreApplyPlan, lock: &RestoreApplyLock) -> String {
    let pre_restore = plan
        .pre_restore_backup
        .as_ref()
        .map(|evidence| evidence.backup_artifact_id.as_str())
        .unwrap_or("<missing>");
    format!(
        "lock_id={}\nplan_id={}\nowner={}\nbackup_artifact_id={}\npre_restore_backup_artifact_id={}\nstatus={}\n",
        lock.lock_id,
        plan.plan_id,
        lock.owner,
        plan.backup_artifact_id,
        pre_restore,
        lock.status
    )
}

fn restore_lock_conflict(plan_id: &str, lock_path: Option<&Path>) -> EvaError {
    let mut error =
        EvaError::conflict("restore apply lock already exists").with_context("plan_id", plan_id);
    if let Some(lock_path) = lock_path {
        error = error.with_context("lock_path", lock_path.display().to_string());
    }
    error
}

fn restore_apply_steps(health_passed: bool) -> Vec<String> {
    let mut steps = vec![
        "match restore apply confirmation to plan id".to_owned(),
        "verify signed backup artifact and pre-restore evidence".to_owned(),
        "require runtime policy approval for restore.apply".to_owned(),
        "acquire restore apply lock".to_owned(),
        "run pre-apply health check".to_owned(),
    ];
    if health_passed {
        steps.push("gate destructive restore execution".to_owned());
        steps.push("emit staged restore apply audit".to_owned());
    } else {
        steps.push("block destructive restore execution".to_owned());
        steps.push("emit rollback-required audit".to_owned());
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_policy::{HighRiskAction, PolicyDecision};

    fn allowed_policy() -> PolicyDecision {
        PolicyDecision {
            action: HighRiskAction::RestoreApply,
            allowed: true,
            reason: "explicitly allowed by test".to_owned(),
            audit: vec!["runtime:restore.apply:allowed".to_owned()],
        }
    }

    fn denied_policy() -> PolicyDecision {
        PolicyDecision {
            action: HighRiskAction::RestoreApply,
            allowed: false,
            reason: "restore.apply requires explicit approval".to_owned(),
            audit: vec!["runtime:restore.apply:denied".to_owned()],
        }
    }

    fn matching_plan_and_artifacts() -> (RestoreApplyPlan, ArtifactRecord, ArtifactRecord) {
        let artifact = ArtifactRecord::new("backup/backup-1", b"ok".as_slice());
        let pre_restore = ArtifactRecord::new("backup/pre-restore-1", b"before".as_slice());
        let plan = RestoreApplyPlan::new("plan-1", "backup-1", artifact.digest.clone())
            .unwrap()
            .with_pre_restore_backup(
                PreRestoreBackupEvidence::new("pre-restore-1", pre_restore.digest.clone()).unwrap(),
            );
        (plan, artifact, pre_restore)
    }

    #[test]
    fn dry_run_rejects_digest_mismatch() {
        let plan = RestoreApplyPlan::new("plan-1", "backup-1", "sha256:wrong").unwrap();
        let artifact = ArtifactRecord::new("backup/backup-1", b"ok".as_slice());
        let pre_restore = ArtifactRecord::new("backup/pre-restore-1", b"before".as_slice());
        let plan = plan.with_pre_restore_backup(
            PreRestoreBackupEvidence::new("pre-restore-1", pre_restore.digest.clone()).unwrap(),
        );

        let error = RestoreApplyValidator
            .dry_run(&plan, &artifact, Some(&pre_restore))
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    fn dry_run_validates_matching_backup_and_pre_restore_evidence() {
        let digest = "sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df";
        let pre_restore = ArtifactRecord::new("backup/pre-restore-1", b"before".as_slice());
        let plan = RestoreApplyPlan::new("plan-1", "backup-1", digest)
            .unwrap()
            .with_pre_restore_backup(
                PreRestoreBackupEvidence::new("pre-restore-1", pre_restore.digest.clone()).unwrap(),
            );
        let artifact = ArtifactRecord::new("backup/backup-1", b"ok".as_slice());

        let report = RestoreApplyValidator
            .dry_run(&plan, &artifact, Some(&pre_restore))
            .unwrap();

        assert_eq!(report.status, "dry_run_validated");
        assert!(!report.apply_allowed);
        assert_eq!(
            report.pre_restore_backup_artifact_key,
            "backup/pre-restore-1"
        );
    }

    #[test]
    fn dry_run_requires_pre_restore_evidence() {
        let digest = "sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df";
        let plan = RestoreApplyPlan::new("plan-1", "backup-1", digest).unwrap();
        let artifact = ArtifactRecord::new("backup/backup-1", b"ok".as_slice());

        let error = RestoreApplyValidator
            .dry_run(&plan, &artifact, None)
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
    }

    #[test]
    fn restore_apply_gate_requires_policy_approval() {
        let (plan, artifact, pre_restore) = matching_plan_and_artifacts();
        let dry_run = RestoreApplyValidator
            .dry_run(&plan, &artifact, Some(&pre_restore))
            .unwrap();
        let mut store = InMemoryRestoreApplyLockStore::new();

        let error = RestoreApplyCoordinator
            .apply(
                &mut store,
                &plan,
                &dry_run,
                &denied_policy(),
                RestoreApplyHealthCheck::healthy(),
                "cli",
            )
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::PermissionDenied);
    }

    #[test]
    fn restore_apply_gate_acquires_lock_after_evidence_policy_and_health() {
        let (plan, artifact, pre_restore) = matching_plan_and_artifacts();
        let dry_run = RestoreApplyValidator
            .dry_run(&plan, &artifact, Some(&pre_restore))
            .unwrap();
        let mut store = InMemoryRestoreApplyLockStore::new();

        let report = RestoreApplyCoordinator
            .apply(
                &mut store,
                &plan,
                &dry_run,
                &allowed_policy(),
                RestoreApplyHealthCheck::healthy(),
                "cli",
            )
            .unwrap();

        assert_eq!(report.status, "gated");
        assert!(report.apply_allowed);
        assert!(!report.mutation_executed);
        assert_eq!(report.lock.lock_id, "restore-apply-plan-1");
    }

    #[test]
    fn restore_apply_gate_reports_lock_conflict() {
        let (plan, artifact, pre_restore) = matching_plan_and_artifacts();
        let dry_run = RestoreApplyValidator
            .dry_run(&plan, &artifact, Some(&pre_restore))
            .unwrap();
        let mut store = InMemoryRestoreApplyLockStore::new();

        RestoreApplyCoordinator
            .apply(
                &mut store,
                &plan,
                &dry_run,
                &allowed_policy(),
                RestoreApplyHealthCheck::healthy(),
                "cli",
            )
            .unwrap();
        let error = RestoreApplyCoordinator
            .apply(
                &mut store,
                &plan,
                &dry_run,
                &allowed_policy(),
                RestoreApplyHealthCheck::healthy(),
                "cli",
            )
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    fn restore_apply_health_failure_blocks_apply_after_lock() {
        let (plan, artifact, pre_restore) = matching_plan_and_artifacts();
        let dry_run = RestoreApplyValidator
            .dry_run(&plan, &artifact, Some(&pre_restore))
            .unwrap();
        let mut store = InMemoryRestoreApplyLockStore::new();

        let report = RestoreApplyCoordinator
            .apply(
                &mut store,
                &plan,
                &dry_run,
                &allowed_policy(),
                RestoreApplyHealthCheck::failed("pre-restore health failed").unwrap(),
                "cli",
            )
            .unwrap();

        assert_eq!(report.status, "blocked");
        assert!(!report.apply_allowed);
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "restore.apply:rollback_required"));
    }
}
