//! Backup, migration package, and release snapshot boundary.

pub mod backup_service;
pub mod manifest_verifier;
pub mod migration_package;
pub mod release_snapshot;

pub use backup_service::{
    BackupEntry, BackupManifest, BackupManifestEntry, BackupPlan, BackupResult, BackupScope,
    BackupService,
};
pub use manifest_verifier::{ManifestVerifier, VerificationReport};
pub use migration_package::{
    MigrationPackageManifest, MigrationPackageService, MigrationPreflight,
};
pub use release_snapshot::{ReleaseSnapshot, ReleaseSnapshotService, RestorePlan, SnapshotRole};
