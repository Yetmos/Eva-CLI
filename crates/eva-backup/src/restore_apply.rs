//! Restore apply validation, staged mutation planning, lock, policy, and health gate boundaries.

use crate::archive::digest_bytes;
use crate::manifest_verifier::ManifestVerifier;
use eva_core::EvaError;
use eva_policy::PolicyDecision;
use eva_storage::ArtifactRecord;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

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
    pub mutation_target_root: String,
    pub mutation_steps: Vec<RestoreMutationStep>,
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
    pub mutation_plan: RestoreStagedMutationPlan,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreMutationOperation {
    Copy,
    Delete,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreMutationTargetKind {
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreMutationStep {
    pub operation: RestoreMutationOperation,
    pub relative_path: String,
    pub source_artifact_key: Option<String>,
    pub expected_digest: Option<String>,
    pub pre_restore_digest: Option<String>,
    pub target_kind: RestoreMutationTargetKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreRollbackEntry {
    pub relative_path: String,
    pub action: String,
    pub pre_restore_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreStagedMutationPlan {
    pub plan_id: String,
    pub target_root: String,
    pub mutation_planned: bool,
    pub mutation_executed: bool,
    pub steps: Vec<RestoreMutationStep>,
    pub affected_paths: Vec<String>,
    pub preview: Vec<String>,
    pub preflight_hash: String,
    pub rollback_manifest: Vec<RestoreRollbackEntry>,
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
    pub mutation_plan: RestoreStagedMutationPlan,
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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RestoreStagedMutationPlanner;

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
            mutation_target_root: ".".to_owned(),
            mutation_steps: Vec::new(),
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

    pub fn with_mutation_target_root(
        mut self,
        target_root: impl Into<String>,
    ) -> Result<Self, EvaError> {
        self.mutation_target_root = validate_restore_target_root(target_root.into())?;
        Ok(self)
    }

    pub fn with_mutation_steps(mut self, steps: Vec<RestoreMutationStep>) -> Self {
        self.mutation_steps = steps;
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

impl RestoreMutationOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Delete => "delete",
            Self::Replace => "replace",
        }
    }

    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "copy" => Ok(Self::Copy),
            "delete" => Ok(Self::Delete),
            "replace" => Ok(Self::Replace),
            _ => Err(EvaError::invalid_argument(
                "restore mutation operation must be copy, delete, or replace",
            )
            .with_context("operation", value)),
        }
    }
}

impl RestoreMutationTargetKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
        }
    }

    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "file" => Ok(Self::File),
            "symlink" => Err(EvaError::invalid_argument(
                "restore mutation plan rejects symlink targets",
            )
            .with_context("target_kind", value)),
            _ => Err(
                EvaError::invalid_argument("restore mutation target kind must be file")
                    .with_context("target_kind", value),
            ),
        }
    }
}

impl RestoreMutationStep {
    pub fn copy_file(
        relative_path: impl Into<String>,
        source_artifact_key: impl Into<String>,
        expected_digest: impl Into<String>,
    ) -> Result<Self, EvaError> {
        Self::new(
            RestoreMutationOperation::Copy,
            relative_path,
            Some(source_artifact_key.into()),
            Some(expected_digest.into()),
            None,
            RestoreMutationTargetKind::File,
        )
    }

    pub fn delete_file(
        relative_path: impl Into<String>,
        pre_restore_digest: impl Into<String>,
    ) -> Result<Self, EvaError> {
        Self::new(
            RestoreMutationOperation::Delete,
            relative_path,
            None,
            None,
            Some(pre_restore_digest.into()),
            RestoreMutationTargetKind::File,
        )
    }

    pub fn replace_file(
        relative_path: impl Into<String>,
        source_artifact_key: impl Into<String>,
        expected_digest: impl Into<String>,
        pre_restore_digest: impl Into<String>,
    ) -> Result<Self, EvaError> {
        Self::new(
            RestoreMutationOperation::Replace,
            relative_path,
            Some(source_artifact_key.into()),
            Some(expected_digest.into()),
            Some(pre_restore_digest.into()),
            RestoreMutationTargetKind::File,
        )
    }

    pub fn new(
        operation: RestoreMutationOperation,
        relative_path: impl Into<String>,
        source_artifact_key: Option<String>,
        expected_digest: Option<String>,
        pre_restore_digest: Option<String>,
        target_kind: RestoreMutationTargetKind,
    ) -> Result<Self, EvaError> {
        let relative_path = validate_restore_relative_path("relative_path", relative_path.into())?;
        let source_artifact_key = match source_artifact_key {
            Some(value) => Some(validate_restore_artifact_key(value)?),
            None => None,
        };
        let expected_digest = match expected_digest {
            Some(value) => Some(validate_restore_digest("expected_digest", value)?),
            None => None,
        };
        let pre_restore_digest = match pre_restore_digest {
            Some(value) => Some(validate_restore_digest("pre_restore_digest", value)?),
            None => None,
        };
        match operation {
            RestoreMutationOperation::Copy => {
                require_some(
                    "source_artifact_key",
                    source_artifact_key.as_deref(),
                    operation,
                    &relative_path,
                )?;
                require_some(
                    "expected_digest",
                    expected_digest.as_deref(),
                    operation,
                    &relative_path,
                )?;
                if pre_restore_digest.is_some() {
                    return Err(EvaError::invalid_argument(
                        "restore copy mutation cannot include a pre-restore digest",
                    )
                    .with_context("relative_path", &relative_path));
                }
            }
            RestoreMutationOperation::Replace => {
                require_some(
                    "source_artifact_key",
                    source_artifact_key.as_deref(),
                    operation,
                    &relative_path,
                )?;
                require_some(
                    "expected_digest",
                    expected_digest.as_deref(),
                    operation,
                    &relative_path,
                )?;
                require_some(
                    "pre_restore_digest",
                    pre_restore_digest.as_deref(),
                    operation,
                    &relative_path,
                )?;
            }
            RestoreMutationOperation::Delete => {
                if source_artifact_key.is_some() || expected_digest.is_some() {
                    return Err(EvaError::invalid_argument(
                        "restore delete mutation cannot include source artifact or expected digest",
                    )
                    .with_context("relative_path", &relative_path));
                }
                require_some(
                    "pre_restore_digest",
                    pre_restore_digest.as_deref(),
                    operation,
                    &relative_path,
                )?;
            }
        }
        Ok(Self {
            operation,
            relative_path,
            source_artifact_key,
            expected_digest,
            pre_restore_digest,
            target_kind,
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
        let mutation_plan = RestoreStagedMutationPlanner.plan(plan)?;
        let mut audit = vec![
            "restore.apply:dry_run".to_owned(),
            "backup:verified".to_owned(),
            "pre_restore_backup:verified".to_owned(),
            "apply_allowed:false".to_owned(),
        ];
        audit.extend(
            mutation_plan
                .audit
                .iter()
                .map(|entry| format!("mutation:{entry}")),
        );
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
            mutation_plan,
            audit,
        })
    }
}

impl RestoreStagedMutationPlanner {
    pub fn plan(&self, plan: &RestoreApplyPlan) -> Result<RestoreStagedMutationPlan, EvaError> {
        let target_root = validate_restore_target_root(plan.mutation_target_root.clone())?;
        let mut affected_paths = BTreeSet::new();
        let mut preview = Vec::new();
        let mut rollback_manifest = Vec::new();
        for step in &plan.mutation_steps {
            affected_paths.insert(step.relative_path.clone());
            preview.push(mutation_preview(step));
            rollback_manifest.push(rollback_entry(step));
        }
        let affected_paths = affected_paths.into_iter().collect::<Vec<_>>();
        let preflight_hash = digest_bytes(
            canonical_mutation_plan_payload(
                plan,
                &target_root,
                &affected_paths,
                &rollback_manifest,
            )
            .as_bytes(),
        );
        let mutation_planned = !plan.mutation_steps.is_empty();
        let mut audit = vec!["restore.mutation:plan_only".to_owned()];
        if mutation_planned {
            audit.push("restore.mutation:staged_steps_validated".to_owned());
            audit.push(format!(
                "restore.mutation:affected_paths={}",
                affected_paths.len()
            ));
            audit.push("mutation_executed:false".to_owned());
        } else {
            audit.push("restore.mutation:no_steps_declared".to_owned());
        }
        Ok(RestoreStagedMutationPlan {
            plan_id: plan.plan_id.clone(),
            target_root,
            mutation_planned,
            mutation_executed: false,
            steps: plan.mutation_steps.clone(),
            affected_paths,
            preview,
            preflight_hash,
            rollback_manifest,
            audit,
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
                mutation_plan: dry_run.mutation_plan.clone(),
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
                mutation_plan: dry_run.mutation_plan.clone(),
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

fn validate_restore_target_root(value: String) -> Result<String, EvaError> {
    if value.trim().is_empty() || value.trim() != value || value.contains('\0') {
        return Err(EvaError::invalid_argument(
            "restore mutation target root must be non-empty and trimmed",
        )
        .with_context("target_root", value));
    }
    if value.contains("..") {
        return Err(EvaError::invalid_argument(
            "restore mutation target root cannot contain parent traversal",
        )
        .with_context("target_root", value));
    }
    Ok(value)
}

fn validate_restore_relative_path(field: &'static str, value: String) -> Result<String, EvaError> {
    if value.trim().is_empty() || value.trim() != value || value.contains('\0') {
        return Err(EvaError::invalid_argument(
            "restore mutation path must be non-empty and trimmed",
        )
        .with_context("field", field)
        .with_context("path", value));
    }
    if value.contains('\\') || value.contains(':') || value.contains('|') {
        return Err(EvaError::invalid_argument(
            "restore mutation path must be a stable forward-slash relative path",
        )
        .with_context("field", field)
        .with_context("path", value));
    }
    if value.split('/').any(|segment| segment.is_empty()) {
        return Err(EvaError::invalid_argument(
            "restore mutation path cannot contain empty components",
        )
        .with_context("field", field)
        .with_context("path", value));
    }
    for component in Path::new(&value).components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(EvaError::invalid_argument(
                    "restore mutation path must stay inside target root",
                )
                .with_context("field", field)
                .with_context("path", value));
            }
        }
    }
    Ok(value)
}

fn validate_restore_artifact_key(value: String) -> Result<String, EvaError> {
    validate_restore_relative_path("source_artifact_key", value)
}

fn validate_restore_digest(field: &'static str, value: String) -> Result<String, EvaError> {
    if !value.starts_with("sha256:") || value.trim() != value {
        return Err(
            EvaError::invalid_argument("restore mutation digest must be sha256")
                .with_context("field", field)
                .with_context("digest", value),
        );
    }
    Ok(value)
}

fn require_some(
    field: &'static str,
    value: Option<&str>,
    operation: RestoreMutationOperation,
    relative_path: &str,
) -> Result<(), EvaError> {
    if value.is_some() {
        return Ok(());
    }
    Err(
        EvaError::invalid_argument("restore mutation step is missing a required field")
            .with_context("field", field)
            .with_context("operation", operation.as_str())
            .with_context("relative_path", relative_path),
    )
}

fn mutation_preview(step: &RestoreMutationStep) -> String {
    match step.operation {
        RestoreMutationOperation::Copy => format!(
            "copy {} from {} expecting {}",
            step.relative_path,
            step.source_artifact_key.as_deref().unwrap_or("<missing>"),
            step.expected_digest.as_deref().unwrap_or("<missing>")
        ),
        RestoreMutationOperation::Delete => format!(
            "delete {} after verifying pre-restore {}",
            step.relative_path,
            step.pre_restore_digest.as_deref().unwrap_or("<missing>")
        ),
        RestoreMutationOperation::Replace => format!(
            "replace {} from {} expecting {} after pre-restore {}",
            step.relative_path,
            step.source_artifact_key.as_deref().unwrap_or("<missing>"),
            step.expected_digest.as_deref().unwrap_or("<missing>"),
            step.pre_restore_digest.as_deref().unwrap_or("<missing>")
        ),
    }
}

fn rollback_entry(step: &RestoreMutationStep) -> RestoreRollbackEntry {
    match step.operation {
        RestoreMutationOperation::Copy => RestoreRollbackEntry {
            relative_path: step.relative_path.clone(),
            action: "delete_restored_path".to_owned(),
            pre_restore_digest: None,
        },
        RestoreMutationOperation::Delete | RestoreMutationOperation::Replace => {
            RestoreRollbackEntry {
                relative_path: step.relative_path.clone(),
                action: "restore_pre_restore_digest".to_owned(),
                pre_restore_digest: step.pre_restore_digest.clone(),
            }
        }
    }
}

fn canonical_mutation_plan_payload(
    plan: &RestoreApplyPlan,
    target_root: &str,
    affected_paths: &[String],
    rollback_manifest: &[RestoreRollbackEntry],
) -> String {
    let mut payload = format!(
        "restore-mutation-plan:v1\nplan_id={}\ntarget_root={}\nbackup_artifact_key={}\npre_restore_backup_artifact_key={}\n",
        plan.plan_id,
        target_root,
        plan.backup_artifact_key(),
        plan.pre_restore_backup
            .as_ref()
            .map(PreRestoreBackupEvidence::backup_artifact_key)
            .unwrap_or_else(|| "<missing>".to_owned())
    );
    payload.push_str(&format!("steps={}\n", plan.mutation_steps.len()));
    for (index, step) in plan.mutation_steps.iter().enumerate() {
        payload.push_str(&format!(
            "step[{index}]={}|{}|{}|{}|{}|{}\n",
            step.operation.as_str(),
            step.relative_path,
            step.source_artifact_key.as_deref().unwrap_or("none"),
            step.expected_digest.as_deref().unwrap_or("none"),
            step.pre_restore_digest.as_deref().unwrap_or("none"),
            step.target_kind.as_str()
        ));
    }
    payload.push_str(&format!("affected_paths={}\n", affected_paths.len()));
    for path in affected_paths {
        payload.push_str(&format!("affected={path}\n"));
    }
    payload.push_str(&format!("rollback_entries={}\n", rollback_manifest.len()));
    for (index, entry) in rollback_manifest.iter().enumerate() {
        payload.push_str(&format!(
            "rollback[{index}]={}|{}|{}\n",
            entry.relative_path,
            entry.action,
            entry.pre_restore_digest.as_deref().unwrap_or("none")
        ));
    }
    payload.push_str("mutation_executed=false\n");
    payload
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
        assert!(!report.mutation_plan.mutation_executed);
        assert_eq!(report.mutation_plan.audit[0], "restore.mutation:plan_only");
    }

    #[test]
    fn staged_mutation_planner_builds_reproducible_preview_and_rollback_manifest() {
        let artifact = ArtifactRecord::new("backup/backup-1", b"archive".as_slice());
        let pre_restore = ArtifactRecord::new("backup/pre-restore-1", b"before".as_slice());
        let copy_digest = ArtifactRecord::new("backup/config", b"config".as_slice()).digest;
        let replace_digest = ArtifactRecord::new("backup/bin", b"binary".as_slice()).digest;
        let delete_digest = ArtifactRecord::new("backup/log", b"log".as_slice()).digest;
        let replaced_digest =
            ArtifactRecord::new("backup/old-bin", b"old-binary".as_slice()).digest;
        let plan = RestoreApplyPlan::new("plan-1", "backup-1", artifact.digest)
            .unwrap()
            .with_pre_restore_backup(
                PreRestoreBackupEvidence::new("pre-restore-1", pre_restore.digest).unwrap(),
            )
            .with_mutation_target_root("workspace")
            .unwrap()
            .with_mutation_steps(vec![
                RestoreMutationStep::copy_file(
                    "config/eva.yaml",
                    "backup/config",
                    copy_digest.clone(),
                )
                .unwrap(),
                RestoreMutationStep::delete_file("logs/old.log", delete_digest.clone()).unwrap(),
                RestoreMutationStep::replace_file(
                    "bin/eva",
                    "backup/bin",
                    replace_digest.clone(),
                    replaced_digest.clone(),
                )
                .unwrap(),
            ]);

        let staged = RestoreStagedMutationPlanner.plan(&plan).unwrap();
        let staged_again = RestoreStagedMutationPlanner.plan(&plan).unwrap();

        assert!(staged.mutation_planned);
        assert!(!staged.mutation_executed);
        assert_eq!(staged.preflight_hash, staged_again.preflight_hash);
        assert_eq!(
            staged.affected_paths,
            vec![
                "bin/eva".to_owned(),
                "config/eva.yaml".to_owned(),
                "logs/old.log".to_owned()
            ]
        );
        assert_eq!(staged.preview.len(), 3);
        assert!(staged.preview[0].contains("copy config/eva.yaml"));
        assert_eq!(staged.rollback_manifest[0].action, "delete_restored_path");
        assert_eq!(
            staged.rollback_manifest[1].pre_restore_digest.as_deref(),
            Some(delete_digest.as_str())
        );
        assert_eq!(
            staged.rollback_manifest[2].pre_restore_digest.as_deref(),
            Some(replaced_digest.as_str())
        );
    }

    #[test]
    fn staged_mutation_planner_rejects_path_escape_and_symlink_targets() {
        let digest = ArtifactRecord::new("backup/config", b"config".as_slice()).digest;

        let traversal =
            RestoreMutationStep::copy_file("../secret", "backup/config", digest.clone())
                .unwrap_err();
        assert_eq!(traversal.kind(), eva_core::ErrorKind::InvalidArgument);

        let absolute =
            RestoreMutationStep::copy_file("/tmp/secret", "backup/config", digest).unwrap_err();
        assert_eq!(absolute.kind(), eva_core::ErrorKind::InvalidArgument);

        let windows_prefix = RestoreMutationStep::delete_file(
            "C:/eva/secret",
            ArtifactRecord::new("backup/secret", b"secret".as_slice()).digest,
        )
        .unwrap_err();
        assert_eq!(windows_prefix.kind(), eva_core::ErrorKind::InvalidArgument);

        let symlink = RestoreMutationTargetKind::parse("symlink").unwrap_err();
        assert_eq!(symlink.kind(), eva_core::ErrorKind::InvalidArgument);
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
