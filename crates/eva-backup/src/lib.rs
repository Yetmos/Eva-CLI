//! Backup, migration package, and release snapshot boundary.

pub mod archive;
pub mod backup_service;
pub mod manifest_verifier;
pub mod migration_package;
pub mod release_snapshot;
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
};
