//! Backup service orchestration.

use crate::manifest_verifier::{ManifestVerifier, VerificationReport};
use eva_core::{EvaError, GenerationId, RequestId};
use eva_storage::{ArtifactRecord, ArtifactStore};

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
        if path.trim().is_empty() || path.contains("..") || path.contains('\\') {
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
        })
    }

    pub fn dry_run(mut self) -> Self {
        self.dry_run = true;
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
        let payload = backup_payload(&plan);
        let artifact = store.put_bytes(artifact_key, payload.into_bytes())?;
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
            audit: vec![
                "backup:created".to_owned(),
                format!("artifact:{}", artifact.key),
                format!("dry_run:{}", plan.dry_run),
            ],
        };
        let verification = ManifestVerifier::verify_artifact(&artifact, &manifest.digest)?;
        Ok(BackupResult {
            plan,
            manifest,
            artifact,
            verification,
        })
    }
}

fn backup_payload(plan: &BackupPlan) -> String {
    let mut payload = format!(
        "artifact={}\nrequest={}\ngeneration={}\nproject={}\ncreated_by={}\nreason={}\n",
        plan.artifact_id,
        plan.request_id.as_str(),
        plan.runtime_generation.as_str(),
        plan.scope.project_id,
        plan.created_by,
        plan.reason
    );
    for entry in &plan.scope.entries {
        payload.push_str(&format!(
            "entry={} size={} redacted={}\n",
            entry.path,
            entry.bytes.len(),
            entry.redacted
        ));
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(result.manifest.entries[0].path, "config/eva.yaml");
        assert_eq!(
            store.get_bytes(&result.artifact.key).unwrap().digest,
            result.manifest.digest
        );
    }
}
