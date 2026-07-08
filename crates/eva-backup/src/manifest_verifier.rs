//! Artifact and manifest integrity verification.

use crate::archive::{verify_record_checksum, BackupArchiveVerifier, BackupSigningKey};
use crate::backup_service::BackupManifest;
use eva_core::EvaError;
use eva_storage::ArtifactRecord;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "artifact and manifest integrity verification";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationReport {
    pub artifact_key: String,
    pub expected_digest: String,
    pub actual_digest: String,
    pub verified: bool,
    pub audit: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ManifestVerifier;

impl ManifestVerifier {
    pub fn verify_artifact(
        record: &ArtifactRecord,
        expected_digest: &str,
    ) -> Result<VerificationReport, EvaError> {
        if expected_digest.trim().is_empty() {
            return Err(
                EvaError::invalid_argument("expected digest cannot be empty")
                    .with_context("artifact_key", &record.key),
            );
        }
        verify_record_checksum(record, expected_digest)?;
        Ok(VerificationReport {
            artifact_key: record.key.clone(),
            expected_digest: expected_digest.to_owned(),
            actual_digest: record.digest.clone(),
            verified: true,
            audit: vec![
                "manifest:verified".to_owned(),
                format!("artifact:{}", record.key),
            ],
        })
    }

    pub fn verify_backup_archive(
        record: &ArtifactRecord,
        manifest: &BackupManifest,
        signing_key: &BackupSigningKey,
    ) -> Result<VerificationReport, EvaError> {
        if manifest.archive.artifact_key != record.key {
            return Err(EvaError::conflict("backup archive artifact key mismatch")
                .with_context("expected_artifact_key", &manifest.archive.artifact_key)
                .with_context("actual_artifact_key", &record.key));
        }
        if manifest.archive.checksum != manifest.digest {
            return Err(
                EvaError::conflict("backup archive manifest checksum mismatch")
                    .with_context("artifact_key", &record.key)
                    .with_context("manifest_digest", &manifest.digest)
                    .with_context("archive_checksum", &manifest.archive.checksum),
            );
        }
        let mut report = Self::verify_artifact(record, &manifest.digest)?;
        let signature = BackupArchiveVerifier::verify_signature(&manifest.archive, signing_key)?;
        report.audit.push("archive:verified".to_owned());
        report.audit.push(format!("signature:{}", signature.key_id));
        if manifest.archive.encrypted {
            report.audit.push("archive:encrypted".to_owned());
        }
        if manifest.archive.remote_target.is_some() {
            report.audit.push("remote_target:declared".to_owned());
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_rejects_digest_mismatch() {
        let record = ArtifactRecord::new("backup/test", b"ok".as_slice());

        let error = ManifestVerifier::verify_artifact(&record, "sha256:wrong").unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    fn verifier_rejects_corrupt_record_bytes() {
        let mut record = ArtifactRecord::new("backup/test", b"ok".as_slice());
        let expected = record.digest.clone();
        record.bytes = b"tampered".to_vec();

        let error = ManifestVerifier::verify_artifact(&record, &expected).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert!(error.message().contains("bytes digest"));
    }
}
