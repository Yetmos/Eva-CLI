//! Backup, migration package, and release snapshot boundary.

pub mod backup_service;
pub mod manifest_verifier;
pub mod migration_package;
pub mod release_snapshot;
pub mod restore_apply;

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
pub use restore_apply::{RestoreApplyDryRunReport, RestoreApplyPlan, RestoreApplyValidator};
