//! 备份、迁移包和发布快照边界。
//! Backup, migration package, and release snapshot boundary.

/// 备份归档的签名、可选密封与远端目标契约。
pub mod archive;
/// 备份计划、归档生成和持久化编排。
pub mod backup_service;
/// 归档与清单的完整性验证。
pub mod manifest_verifier;
/// 迁移包清单与执行前检查。
pub mod migration_package;
/// 发布前后快照及恢复规划。
pub mod release_snapshot;
/// 受门禁控制的恢复应用、事务日志和失败回滚。
pub mod restore_apply;

pub use archive::{
    BackupArchiveCodec, BackupArchiveManifest, BackupArchiveVerifier, BackupEncryptionKey,
    BackupEncryptionManifest, BackupSignatureManifest, BackupSignatureVerification,
    BackupSigningKey, RemoteBackupTarget, RemoteBackupTargetKind, SealedBackupArchive,
};
pub use backup_service::{
    BackupEntry, BackupManifest, BackupManifestEntry, BackupPlan, BackupResult, BackupScope,
    BackupService,
};
pub use manifest_verifier::{ManifestVerifier, VerificationReport};
pub use migration_package::{
    MigrationPackageManifest, MigrationPackageService, MigrationPreflight,
};
pub use release_snapshot::{
    ReleasePointerPlan, ReleaseSnapshot, ReleaseSnapshotService, RestorePlan, SnapshotRole,
};
pub use restore_apply::{
    FileSystemRestoreApplyLockStore, InMemoryRestoreApplyLockStore, PreRestoreBackupEvidence,
    RestoreApplyCoordinator, RestoreApplyDryRunReport, RestoreApplyHealthCheck, RestoreApplyLock,
    RestoreApplyLockStore, RestoreApplyPlan, RestoreApplyReport, RestoreApplyValidator,
    RestoreMutationApplyReport, RestoreMutationEngine, RestoreMutationOperation,
    RestoreMutationStep, RestoreMutationTargetKind, RestoreMutationTransactionEntry,
    RestorePreRestoreArchive, RestorePreRestoreArchiveEntry, RestoreRollbackApplyReport,
    RestoreRollbackEngine, RestoreRollbackEntry, RestoreStagedMutationPlan,
    RestoreStagedMutationPlanner,
};
