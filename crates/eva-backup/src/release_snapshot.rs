//! Release snapshot generation and restore boundaries.

use crate::backup_service::BackupManifest;
use eva_core::{EvaError, GenerationId, RequestId};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "release snapshot generation and restore boundaries";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotRole {
    PreRelease,
    PostRelease,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseSnapshot {
    pub snapshot_id: String,
    pub role: SnapshotRole,
    pub release_ref: String,
    pub request_id: RequestId,
    pub runtime_generation: GenerationId,
    pub backup_artifact_id: String,
    pub backup_digest: String,
    pub health_status: String,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePlan {
    pub snapshot_id: String,
    pub status: String,
    pub apply_allowed: bool,
    pub steps: Vec<String>,
    pub risks: Vec<String>,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasePointerPlan {
    pub snapshot_id: String,
    pub release_ref: String,
    pub runtime_generation: GenerationId,
    pub pointer_path: String,
    pub status: String,
    pub apply_allowed: bool,
    pub steps: Vec<String>,
    pub risks: Vec<String>,
    pub audit: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReleaseSnapshotService;

impl SnapshotRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreRelease => "pre_release",
            Self::PostRelease => "post_release",
        }
    }
}

impl ReleaseSnapshotService {
    pub fn create(
        &self,
        snapshot_id: impl Into<String>,
        role: SnapshotRole,
        release_ref: impl Into<String>,
        request_id: RequestId,
        backup: &BackupManifest,
        health_status: impl Into<String>,
    ) -> Result<ReleaseSnapshot, EvaError> {
        let snapshot_id = snapshot_id.into();
        let release_ref = release_ref.into();
        let health_status = health_status.into();
        if snapshot_id.trim().is_empty()
            || release_ref.trim().is_empty()
            || health_status.trim().is_empty()
        {
            return Err(EvaError::invalid_argument(
                "snapshot id, release ref, and health are required",
            ));
        }
        Ok(ReleaseSnapshot {
            snapshot_id,
            role,
            release_ref,
            request_id,
            runtime_generation: backup.runtime_generation.clone(),
            backup_artifact_id: backup.artifact_id.clone(),
            backup_digest: backup.digest.clone(),
            health_status,
            audit: vec![
                "snapshot:created".to_owned(),
                format!("role:{}", role.as_str()),
            ],
        })
    }

    pub fn restore_plan(&self, snapshot: &ReleaseSnapshot) -> RestorePlan {
        RestorePlan {
            snapshot_id: snapshot.snapshot_id.clone(),
            status: "planned".to_owned(),
            apply_allowed: false,
            steps: vec![
                "verify snapshot manifest and backup digest".to_owned(),
                "acquire lifecycle operation lease".to_owned(),
                "drain current runtime generation".to_owned(),
                "stage restore from backup artifact".to_owned(),
                "run post-restore health verification".to_owned(),
            ],
            risks: vec![
                "restore is plan-first in V1.4; no destructive mutation is executed".to_owned(),
                format!("backup_digest:{}", snapshot.backup_digest),
            ],
            audit: vec!["restore:planned".to_owned()],
        }
    }

    pub fn release_pointer_plan(
        &self,
        snapshot: &ReleaseSnapshot,
        confirm_snapshot_id: &str,
    ) -> Result<ReleasePointerPlan, EvaError> {
        if confirm_snapshot_id != snapshot.snapshot_id {
            return Err(EvaError::permission_denied(
                "snapshot promote confirmation does not match snapshot id",
            )
            .with_context("confirm", confirm_snapshot_id)
            .with_context("snapshot_id", &snapshot.snapshot_id));
        }
        Ok(ReleasePointerPlan {
            snapshot_id: snapshot.snapshot_id.clone(),
            release_ref: snapshot.release_ref.clone(),
            runtime_generation: snapshot.runtime_generation.clone(),
            pointer_path: "state/release-pointer".to_owned(),
            status: "planned".to_owned(),
            apply_allowed: false,
            steps: vec![
                "verify snapshot backup digest before pointer move".to_owned(),
                "acquire lifecycle release pointer lease".to_owned(),
                "stage release pointer update".to_owned(),
                "emit pointer change audit before apply".to_owned(),
            ],
            risks: vec![
                "snapshot promote is plan-first; release pointer is not moved".to_owned(),
                format!("backup_digest:{}", snapshot.backup_digest),
            ],
            audit: vec![
                "snapshot.promote:planned".to_owned(),
                format!("snapshot:{}", snapshot.snapshot_id),
                format!("release:{}", snapshot.release_ref),
                "apply_allowed:false".to_owned(),
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup_service::{BackupEntry, BackupPlan, BackupScope, BackupService};
    use eva_storage::InMemoryArtifactStore;

    #[test]
    fn snapshot_restore_is_plan_first() {
        let scope = BackupScope::new(
            "eva-cli",
            vec![BackupEntry::new("config/eva.yaml", "runtime: basic").unwrap()],
        )
        .unwrap();
        let plan = BackupPlan::new(
            "backup-v14",
            RequestId::parse("req-backup-1").unwrap(),
            GenerationId::parse("gen-v14").unwrap(),
            "cli",
            "pre-release",
            scope,
        )
        .unwrap();
        let mut store = InMemoryArtifactStore::new();
        let backup = BackupService.create(plan, &mut store).unwrap();

        let snapshot = ReleaseSnapshotService
            .create(
                "snap-v14",
                SnapshotRole::PreRelease,
                "1.4.0",
                RequestId::parse("req-snapshot-1").unwrap(),
                &backup.manifest,
                "healthy",
            )
            .unwrap();
        let restore = ReleaseSnapshotService.restore_plan(&snapshot);

        assert_eq!(restore.status, "planned");
        assert!(!restore.apply_allowed);
    }

    #[test]
    fn snapshot_promote_builds_release_pointer_plan() {
        let scope = BackupScope::new(
            "eva-cli",
            vec![BackupEntry::new("config/eva.yaml", "runtime: basic").unwrap()],
        )
        .unwrap();
        let plan = BackupPlan::new(
            "backup-v14",
            RequestId::parse("req-backup-1").unwrap(),
            GenerationId::parse("gen-v14").unwrap(),
            "cli",
            "pre-release",
            scope,
        )
        .unwrap();
        let mut store = InMemoryArtifactStore::new();
        let backup = BackupService.create(plan, &mut store).unwrap();

        let snapshot = ReleaseSnapshotService
            .create(
                "snap-v14",
                SnapshotRole::PostRelease,
                "1.4.0",
                RequestId::parse("req-snapshot-1").unwrap(),
                &backup.manifest,
                "healthy",
            )
            .unwrap();
        let pointer_plan = ReleaseSnapshotService
            .release_pointer_plan(&snapshot, "snap-v14")
            .unwrap();

        assert_eq!(pointer_plan.status, "planned");
        assert!(!pointer_plan.apply_allowed);
        assert_eq!(pointer_plan.pointer_path, "state/release-pointer");
    }

    #[test]
    fn snapshot_promote_requires_matching_confirmation() {
        let scope = BackupScope::new(
            "eva-cli",
            vec![BackupEntry::new("config/eva.yaml", "runtime: basic").unwrap()],
        )
        .unwrap();
        let plan = BackupPlan::new(
            "backup-v14",
            RequestId::parse("req-backup-1").unwrap(),
            GenerationId::parse("gen-v14").unwrap(),
            "cli",
            "pre-release",
            scope,
        )
        .unwrap();
        let mut store = InMemoryArtifactStore::new();
        let backup = BackupService.create(plan, &mut store).unwrap();
        let snapshot = ReleaseSnapshotService
            .create(
                "snap-v14",
                SnapshotRole::PostRelease,
                "1.4.0",
                RequestId::parse("req-snapshot-1").unwrap(),
                &backup.manifest,
                "healthy",
            )
            .unwrap();

        let error = ReleaseSnapshotService
            .release_pointer_plan(&snapshot, "wrong")
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::PermissionDenied);
    }
}
