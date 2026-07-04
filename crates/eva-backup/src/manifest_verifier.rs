//! Artifact and manifest integrity verification.

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
        if record.digest != expected_digest {
            return Err(EvaError::conflict("artifact digest mismatch")
                .with_context("artifact_key", &record.key)
                .with_context("expected_digest", expected_digest)
                .with_context("actual_digest", &record.digest));
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_rejects_digest_mismatch() {
        let record = ArtifactRecord {
            key: "backup/test".to_owned(),
            bytes: b"ok".to_vec(),
            digest: "len:2:sum:218".to_owned(),
        };

        let error = ManifestVerifier::verify_artifact(&record, "len:1:sum:1").unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }
}
