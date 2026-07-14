//! 发布快照生成、恢复与发布指针规划边界。
//! Release snapshot generation and restore boundaries.

use crate::backup_service::BackupManifest;
use eva_core::{EvaError, GenerationId, RequestId};

/// 本模块的架构职责：将发布快照绑定到已验证备份，并保持恢复与提升计划优先。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "release snapshot generation and restore boundaries";

/// 快照相对于发布动作的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotRole {
    /// 在发布变更前创建的回退检查点。
    PreRelease,
    /// 发布完成后记录的健康状态检查点。
    PostRelease,
}

/// 将发布、运行时代际和备份工件绑定的快照记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseSnapshot {
    /// 快照的稳定标识。
    pub snapshot_id: String,
    /// 快照在发布流程中的角色。
    pub role: SnapshotRole,
    /// 快照对应的发布引用。
    pub release_ref: String,
    /// 创建快照的请求标识。
    pub request_id: RequestId,
    /// 备份内容对应的运行时代际。
    pub runtime_generation: GenerationId,
    /// 快照引用的备份工件标识。
    pub backup_artifact_id: String,
    /// 快照创建时绑定的备份存储摘要。
    pub backup_digest: String,
    /// 创建快照时的运行时健康状态文本。
    pub health_status: String,
    /// 快照角色和创建动作的审计记录。
    pub audit: Vec<String>,
}

/// 从快照恢复的非破坏性执行计划。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePlan {
    /// 被恢复的快照标识。
    pub snapshot_id: String,
    /// 当前阶段状态，创建时为 `planned`。
    pub status: String,
    /// 是否已满足破坏性应用门禁；计划阶段固定为 `false`。
    pub apply_allowed: bool,
    /// 后续验证、租约、排空、暂存和健康检查步骤。
    pub steps: Vec<String>,
    /// 恢复边界和备份摘要提示。
    pub risks: Vec<String>,
    /// 恢复规划审计记录。
    pub audit: Vec<String>,
}

/// 将活动发布指针指向快照代际的非破坏性计划。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasePointerPlan {
    /// 被提升的快照标识。
    pub snapshot_id: String,
    /// 快照绑定的发布引用。
    pub release_ref: String,
    /// 快照绑定的运行时代际。
    pub runtime_generation: GenerationId,
    /// 计划修改的发布指针路径。
    pub pointer_path: String,
    /// 当前计划状态。
    pub status: String,
    /// 是否已允许实际指针写入；本服务生成时固定为 `false`。
    pub apply_allowed: bool,
    /// 后续校验、租约和指针暂存步骤。
    pub steps: Vec<String>,
    /// 计划优先边界和备份摘要提示。
    pub risks: Vec<String>,
    /// 快照确认和提升规划的审计记录。
    pub audit: Vec<String>,
}

/// 创建发布快照及非破坏性恢复、提升计划的无状态服务。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReleaseSnapshotService;

impl SnapshotRole {
    /// 返回用于审计的稳定快照角色字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreRelease => "pre_release",
            Self::PostRelease => "post_release",
        }
    }
}

impl ReleaseSnapshotService {
    /// 从已生成备份清单创建与代际、摘要绑定的发布快照。
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

    /// 为快照生成恢复步骤，但保持实际应用门禁关闭。
    ///
    /// 计划要求先验证清单和摘要，再获取生命周期租约、排空当前代际并暂存恢复，
    /// 最后执行健康检查；本方法本身不读取工件也不修改文件。
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

    /// 在显式确认字符串匹配快照标识后生成发布指针计划。
    ///
    /// 匹配确认只防止操作者选错快照，不代表摘要、策略或租约门禁已经通过，因此
    /// 返回计划仍将 `apply_allowed` 设为 `false`，且不会写入指针。
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
/// 快照恢复和发布指针提升的 plan-first 边界测试。
mod tests {
    use super::*;
    use crate::backup_service::{BackupEntry, BackupPlan, BackupScope, BackupService};
    use eva_storage::InMemoryArtifactStore;

    #[test]
    /// 验证恢复计划不会直接允许破坏性应用。
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
    /// 验证匹配确认仅生成发布指针计划而不执行变更。
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
    /// 验证错误快照确认无法生成提升计划。
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
