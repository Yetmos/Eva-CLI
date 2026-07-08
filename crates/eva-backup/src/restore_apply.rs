//! Restore apply dry-run validation boundaries.

use crate::manifest_verifier::ManifestVerifier;
use eva_core::EvaError;
use eva_storage::ArtifactRecord;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "restore apply dry-run validation over durable backup artifacts";

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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RestoreApplyValidator;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
