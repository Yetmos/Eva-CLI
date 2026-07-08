//! Backup service orchestration.

use crate::archive::{
    BackupArchiveCodec, BackupArchiveManifest, BackupEncryptionKey, BackupSigningKey,
    RemoteBackupTarget,
};
use crate::manifest_verifier::{ManifestVerifier, VerificationReport};
use eva_core::{EvaError, GenerationId, RequestId};
use eva_storage::{ArtifactRecord, ArtifactStore};
use std::fmt::Write as _;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "backup service orchestration";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupEntry {
    pub path: String,
    pub bytes: Vec<u8>,
    pub redacted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupScope {
    pub project_id: String,
    pub entries: Vec<BackupEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupPlan {
    pub artifact_id: String,
    pub request_id: RequestId,
    pub runtime_generation: GenerationId,
    pub created_by: String,
    pub reason: String,
    pub scope: BackupScope,
    pub dry_run: bool,
    pub risks: Vec<String>,
    pub signing_key: BackupSigningKey,
    pub encryption_key: Option<BackupEncryptionKey>,
    pub remote_target: Option<RemoteBackupTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupManifestEntry {
    pub path: String,
    pub size_bytes: usize,
    pub redacted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupManifest {
    pub artifact_id: String,
    pub artifact_type: String,
    pub request_id: RequestId,
    pub runtime_generation: GenerationId,
    pub project_id: String,
    pub entries: Vec<BackupManifestEntry>,
    pub digest: String,
    pub archive: BackupArchiveManifest,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupResult {
    pub plan: BackupPlan,
    pub manifest: BackupManifest,
    pub artifact: ArtifactRecord,
    pub verification: VerificationReport,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BackupService;

impl BackupEntry {
    pub fn new(path: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Result<Self, EvaError> {
        let path = path.into();
        if path.trim().is_empty()
            || path.contains("..")
            || path.contains('\\')
            || path.chars().any(char::is_control)
        {
            return Err(
                EvaError::invalid_argument("backup path must be a stable relative path")
                    .with_context("path", path),
            );
        }
        Ok(Self {
            path,
            bytes: bytes.into(),
            redacted: false,
        })
    }

    pub fn redacted(mut self) -> Self {
        self.redacted = true;
        self
    }
}

impl BackupScope {
    pub fn new(project_id: impl Into<String>, entries: Vec<BackupEntry>) -> Result<Self, EvaError> {
        let project_id = project_id.into();
        if project_id.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "backup project id cannot be empty",
            ));
        }
        if entries.is_empty() {
            return Err(EvaError::invalid_argument(
                "backup scope must contain entries",
            ));
        }
        Ok(Self {
            project_id,
            entries,
        })
    }
}

impl BackupPlan {
    pub fn new(
        artifact_id: impl Into<String>,
        request_id: RequestId,
        runtime_generation: GenerationId,
        created_by: impl Into<String>,
        reason: impl Into<String>,
        scope: BackupScope,
    ) -> Result<Self, EvaError> {
        let artifact_id = artifact_id.into();
        let created_by = created_by.into();
        let reason = reason.into();
        if artifact_id.trim().is_empty() || artifact_id.contains('/') || artifact_id.contains('\\')
        {
            return Err(
                EvaError::invalid_argument("backup artifact id must be a stable slug")
                    .with_context("artifact_id", artifact_id),
            );
        }
        if created_by.trim().is_empty() || reason.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "backup actor and reason are required",
            ));
        }
        Ok(Self {
            artifact_id,
            request_id,
            runtime_generation,
            created_by,
            reason,
            scope,
            dry_run: false,
            risks: vec![
                "restore remains plan-first; no destructive mutation is performed".to_owned(),
            ],
            signing_key: BackupSigningKey::local_development(),
            encryption_key: None,
            remote_target: None,
        })
    }

    pub fn dry_run(mut self) -> Self {
        self.dry_run = true;
        self
    }

    pub fn signed_with(mut self, signing_key: BackupSigningKey) -> Self {
        self.signing_key = signing_key;
        self
    }

    pub fn encrypted_with(mut self, encryption_key: BackupEncryptionKey) -> Self {
        self.encryption_key = Some(encryption_key);
        self
    }

    pub fn with_remote_target(mut self, remote_target: RemoteBackupTarget) -> Self {
        self.remote_target = Some(remote_target);
        self
    }
}

impl BackupService {
    pub fn create(
        &self,
        plan: BackupPlan,
        store: &mut impl ArtifactStore,
    ) -> Result<BackupResult, EvaError> {
        let artifact_key = format!("backup/{}", plan.artifact_id);
        let payload = backup_archive_payload(&plan);
        let sealed = BackupArchiveCodec::seal(
            artifact_key.clone(),
            payload,
            &plan.signing_key,
            plan.encryption_key.as_ref(),
            plan.remote_target.clone(),
        );
        let artifact = store.put_bytes(artifact_key, sealed.bytes)?;
        let manifest = BackupManifest {
            artifact_id: plan.artifact_id.clone(),
            artifact_type: "backup".to_owned(),
            request_id: plan.request_id.clone(),
            runtime_generation: plan.runtime_generation.clone(),
            project_id: plan.scope.project_id.clone(),
            entries: plan
                .scope
                .entries
                .iter()
                .map(|entry| BackupManifestEntry {
                    path: entry.path.clone(),
                    size_bytes: entry.bytes.len(),
                    redacted: entry.redacted,
                })
                .collect(),
            digest: artifact.digest.clone(),
            archive: BackupArchiveManifest {
                checksum: artifact.digest.clone(),
                ..sealed.manifest
            },
            audit: vec![
                "backup:created".to_owned(),
                "backup:archive:sealed".to_owned(),
                "backup:signature:created".to_owned(),
                format!("artifact:{}", artifact.key),
                format!("dry_run:{}", plan.dry_run),
            ],
        };
        let verification =
            ManifestVerifier::verify_backup_archive(&artifact, &manifest, &plan.signing_key)?;
        Ok(BackupResult {
            plan,
            manifest,
            artifact,
            verification,
        })
    }
}

fn backup_archive_payload(plan: &BackupPlan) -> Vec<u8> {
    let mut payload = format!(
        "eva-backup-archive:v1\nartifact={}\nrequest={}\ngeneration={}\nproject={}\ncreated_by={}\nreason={}\ndry_run={}\n",
        plan.artifact_id,
        plan.request_id.as_str(),
        plan.runtime_generation.as_str(),
        plan.scope.project_id,
        plan.created_by,
        plan.reason,
        plan.dry_run
    );
    for entry in &plan.scope.entries {
        let _ = writeln!(payload, "entry.path={}", entry.path);
        let _ = writeln!(payload, "entry.size={}", entry.bytes.len());
        let _ = writeln!(payload, "entry.redacted={}", entry.redacted);
        let _ = writeln!(payload, "entry.bytes.hex={}", hex_encode(&entry.bytes));
    }
    payload.into_bytes()
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::{BackupArchiveCodec, BackupEncryptionKey, RemoteBackupTargetKind};
    use eva_storage::InMemoryArtifactStore;

    #[test]
    fn backup_service_creates_and_verifies_artifact() {
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
            "pre-upgrade safety checkpoint",
            scope,
        )
        .unwrap();
        let mut store = InMemoryArtifactStore::new();

        let result = BackupService.create(plan, &mut store).unwrap();

        assert!(result.verification.verified);
        assert_eq!(result.manifest.archive.format, "eva.backup.archive.v1");
        assert_eq!(
            result.manifest.archive.signature.key_id,
            result.plan.signing_key.key_id()
        );
        assert_eq!(result.manifest.entries[0].path, "config/eva.yaml");
        assert_eq!(
            store.get_bytes(&result.artifact.key).unwrap().digest,
            result.manifest.digest
        );
        let archive =
            BackupArchiveCodec::open(&result.artifact, &result.manifest.archive, None).unwrap();
        let archive = String::from_utf8(archive).unwrap();
        assert!(archive.contains("entry.path=config/eva.yaml"));
        assert!(archive.contains("entry.bytes.hex=72756e74696d653a206261736963"));
    }

    #[test]
    fn backup_service_can_encrypt_archive_and_record_remote_target() {
        let encryption_key = BackupEncryptionKey::new("archive-key", "secret").unwrap();
        let remote = RemoteBackupTarget::new(
            RemoteBackupTargetKind::ObjectStore,
            "s3://eva-backups",
            "daily/eva-cli",
        )
        .unwrap()
        .required();
        let scope = BackupScope::new(
            "eva-cli",
            vec![BackupEntry::new("state/release-pointer", "stable").unwrap()],
        )
        .unwrap();
        let plan = BackupPlan::new(
            "backup-v1103",
            RequestId::parse("req-backup-2").unwrap(),
            GenerationId::parse("gen-v1103").unwrap(),
            "cli",
            "pre-restore safety checkpoint",
            scope,
        )
        .unwrap()
        .encrypted_with(encryption_key.clone())
        .with_remote_target(remote);
        let mut store = InMemoryArtifactStore::new();

        let result = BackupService.create(plan, &mut store).unwrap();

        assert!(result.manifest.archive.encrypted);
        assert!(result.manifest.archive.remote_target.is_some());
        assert!(!String::from_utf8_lossy(&result.artifact.bytes).contains("stable"));
        let archive = BackupArchiveCodec::open(
            &result.artifact,
            &result.manifest.archive,
            Some(&encryption_key),
        )
        .unwrap();
        assert!(String::from_utf8(archive)
            .unwrap()
            .contains("entry.path=state/release-pointer"));
    }

    #[test]
    fn backup_signature_mismatch_blocks_verification() {
        let scope = BackupScope::new(
            "eva-cli",
            vec![BackupEntry::new("config/eva.yaml", "runtime: basic").unwrap()],
        )
        .unwrap();
        let plan = BackupPlan::new(
            "backup-signature",
            RequestId::parse("req-backup-3").unwrap(),
            GenerationId::parse("gen-v1103").unwrap(),
            "cli",
            "signature verification",
            scope,
        )
        .unwrap();
        let mut store = InMemoryArtifactStore::new();
        let result = BackupService.create(plan, &mut store).unwrap();
        let wrong_key =
            BackupSigningKey::new(result.plan.signing_key.key_id(), "wrong-secret").unwrap();

        let error =
            ManifestVerifier::verify_backup_archive(&result.artifact, &result.manifest, &wrong_key)
                .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert!(error.message().contains("signature mismatch"));
    }
}
