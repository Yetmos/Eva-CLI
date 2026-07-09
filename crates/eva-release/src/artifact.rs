//! Release artifact evidence and provenance verification contracts.

use eva_core::EvaError;
use eva_storage::ArtifactRecord;
use std::collections::BTreeMap;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "signed release artifact and provenance evidence contract";

pub const ARTIFACT_EVIDENCE_FORMAT: &str = "eva.release.artifact_evidence.v1";
pub const RELEASE_SIGNATURE_ALGORITHM: &str = "sha256-keyed-v1";

#[derive(Clone, PartialEq, Eq)]
pub struct ReleaseArtifactSigningKey {
    key_id: String,
    secret: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseArtifactSignature {
    pub key_id: String,
    pub algorithm: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseArtifactSubject {
    pub name: String,
    pub target: String,
    pub format: String,
    pub binary: String,
    pub digest: String,
    pub size_bytes: u64,
    pub signed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseProvenanceEvidence {
    pub builder: String,
    pub source_commit: String,
    pub build_command: String,
    pub build_profile: String,
    pub sbom: String,
    pub scan_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseArtifactEvidence {
    pub format: String,
    pub version: String,
    pub source_tag: String,
    pub source_commit: String,
    pub artifact: ReleaseArtifactSubject,
    pub provenance: ReleaseProvenanceEvidence,
    pub signature: ReleaseArtifactSignature,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseArtifactVerificationReport {
    pub status: String,
    pub version: String,
    pub source_tag: String,
    pub source_commit: String,
    pub artifact_name: String,
    pub artifact_digest: String,
    pub target: String,
    pub signature_verified: bool,
    pub provenance_verified: bool,
    pub risks: Vec<String>,
    pub audit: Vec<String>,
}

impl ReleaseArtifactSigningKey {
    pub fn new(key_id: impl Into<String>, secret: impl Into<String>) -> Result<Self, EvaError> {
        let key_id = validate_token("release signing key id", key_id.into())?;
        let secret = secret.into();
        if secret.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "release signing key secret cannot be empty",
            ));
        }
        Ok(Self { key_id, secret })
    }

    pub fn local_development() -> Self {
        Self {
            key_id: "eva-local-release-signing-key".to_owned(),
            secret: "eva-local-release-signing-secret".to_owned(),
        }
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }
}

impl std::fmt::Debug for ReleaseArtifactSigningKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReleaseArtifactSigningKey")
            .field("key_id", &self.key_id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl ReleaseArtifactSignature {
    pub fn new(
        key_id: impl Into<String>,
        algorithm: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let key_id = validate_token("release artifact signature key id", key_id.into())?;
        let algorithm = validate_token("release artifact signature algorithm", algorithm.into())?;
        let value = validate_token("release artifact signature value", value.into())?;
        Ok(Self {
            key_id,
            algorithm,
            value,
        })
    }
}

impl ReleaseArtifactSubject {
    pub fn new(
        name: impl Into<String>,
        target: impl Into<String>,
        format: impl Into<String>,
        binary: impl Into<String>,
        digest: impl Into<String>,
        size_bytes: u64,
        signed: bool,
    ) -> Result<Self, EvaError> {
        let name = validate_artifact_name(name.into())?;
        let target = validate_token("release artifact target", target.into())?;
        let format = validate_token("release artifact format", format.into())?;
        let binary = validate_artifact_name(binary.into())?;
        let digest = validate_digest(digest.into())?;
        if size_bytes == 0 {
            return Err(EvaError::invalid_argument(
                "release artifact size must be greater than zero",
            ));
        }
        Ok(Self {
            name,
            target,
            format,
            binary,
            digest,
            size_bytes,
            signed,
        })
    }
}

impl ReleaseProvenanceEvidence {
    pub fn new(
        builder: impl Into<String>,
        source_commit: impl Into<String>,
        build_command: impl Into<String>,
        build_profile: impl Into<String>,
        sbom: impl Into<String>,
        scan_status: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let builder = validate_non_empty("release provenance builder", builder.into())?;
        let source_commit = validate_commit(source_commit.into())?;
        let build_command = validate_non_empty("release build command", build_command.into())?;
        let build_profile = validate_token("release build profile", build_profile.into())?;
        let sbom = validate_non_empty("release SBOM evidence", sbom.into())?;
        let scan_status = validate_token("release scan status", scan_status.into())?;
        Ok(Self {
            builder,
            source_commit,
            build_command,
            build_profile,
            sbom,
            scan_status,
        })
    }
}

impl ReleaseArtifactEvidence {
    pub fn new(
        version: impl Into<String>,
        source_tag: impl Into<String>,
        source_commit: impl Into<String>,
        artifact: ReleaseArtifactSubject,
        provenance: ReleaseProvenanceEvidence,
        signature: ReleaseArtifactSignature,
    ) -> Result<Self, EvaError> {
        let version = validate_version(version.into())?;
        let source_tag = validate_token("release source tag", source_tag.into())?;
        let source_commit = validate_commit(source_commit.into())?;
        Ok(Self {
            format: ARTIFACT_EVIDENCE_FORMAT.to_owned(),
            version,
            source_tag,
            source_commit,
            artifact,
            provenance,
            signature,
        })
    }

    pub fn parse_manifest(data: &str) -> Result<Self, EvaError> {
        let fields = parse_key_value_manifest(data)?;
        let artifact = ReleaseArtifactSubject::new(
            required(&fields, "artifact.name")?,
            required(&fields, "artifact.target")?,
            required(&fields, "artifact.format")?,
            required(&fields, "artifact.binary")?,
            required(&fields, "artifact.digest")?,
            parse_size(required(&fields, "artifact.size_bytes")?)?,
            parse_bool(required(&fields, "artifact.signed")?, "artifact.signed")?,
        )?;
        let provenance = ReleaseProvenanceEvidence::new(
            required(&fields, "provenance.builder")?,
            required(&fields, "provenance.source_commit")?,
            required(&fields, "provenance.build_command")?,
            required(&fields, "provenance.build_profile")?,
            required(&fields, "provenance.sbom")?,
            required(&fields, "provenance.scan_status")?,
        )?;
        let signature = ReleaseArtifactSignature::new(
            required(&fields, "signature.key_id")?,
            required(&fields, "signature.algorithm")?,
            required(&fields, "signature.value")?,
        )?;
        let evidence = Self::new(
            required(&fields, "version")?,
            required(&fields, "source_tag")?,
            required(&fields, "source_commit")?,
            artifact,
            provenance,
            signature,
        )?;
        if required(&fields, "format")? != ARTIFACT_EVIDENCE_FORMAT {
            return Err(
                EvaError::invalid_argument("unsupported release artifact evidence format")
                    .with_context("format", required(&fields, "format")?),
            );
        }
        Ok(evidence)
    }

    pub fn sign(&self, signing_key: &ReleaseArtifactSigningKey) -> ReleaseArtifactSignature {
        ReleaseArtifactSignature {
            key_id: signing_key.key_id.clone(),
            algorithm: RELEASE_SIGNATURE_ALGORITHM.to_owned(),
            value: keyed_digest(&self.signature_payload(), signing_key),
        }
    }

    pub fn verify(
        &self,
        signing_key: &ReleaseArtifactSigningKey,
    ) -> ReleaseArtifactVerificationReport {
        let expected_signature = self.sign(signing_key);
        let signature_verified = self.artifact.signed
            && self.signature.key_id == expected_signature.key_id
            && self.signature.algorithm == expected_signature.algorithm
            && self.signature.value == expected_signature.value;
        let provenance_verified = self.provenance.source_commit == self.source_commit
            && self.provenance.scan_status == "passed"
            && !self.provenance.sbom.trim().is_empty();

        let mut risks = Vec::new();
        if !self.artifact.signed {
            risks.push("release artifact is marked unsigned".to_owned());
        }
        if self.signature.key_id != expected_signature.key_id {
            risks.push("release artifact signature key id mismatch".to_owned());
        }
        if self.signature.algorithm != expected_signature.algorithm {
            risks.push("release artifact signature algorithm mismatch".to_owned());
        }
        if self.signature.value != expected_signature.value {
            risks.push("release artifact signature value mismatch".to_owned());
        }
        if self.provenance.source_commit != self.source_commit {
            risks.push(
                "release provenance source commit does not match artifact evidence".to_owned(),
            );
        }
        if self.provenance.scan_status != "passed" {
            risks.push(format!(
                "release scan status is {}",
                self.provenance.scan_status
            ));
        }

        let status = if signature_verified && provenance_verified {
            "verified"
        } else {
            "blocked"
        }
        .to_owned();

        ReleaseArtifactVerificationReport {
            status,
            version: self.version.clone(),
            source_tag: self.source_tag.clone(),
            source_commit: self.source_commit.clone(),
            artifact_name: self.artifact.name.clone(),
            artifact_digest: self.artifact.digest.clone(),
            target: self.artifact.target.clone(),
            signature_verified,
            provenance_verified,
            risks,
            audit: vec![
                "release.artifact:manifest_parsed".to_owned(),
                format!("release.artifact:{}", self.artifact.name),
                format!("release.artifact.digest:{}", self.artifact.digest),
                format!("release.artifact.signature:{}", self.signature.key_id),
                format!("release.provenance.builder:{}", self.provenance.builder),
                format!("release.provenance.source_commit:{}", self.source_commit),
            ],
        }
    }

    pub fn to_manifest(&self) -> String {
        format!(
            "format={}\nversion={}\nsource_tag={}\nsource_commit={}\nartifact.name={}\nartifact.target={}\nartifact.format={}\nartifact.binary={}\nartifact.digest={}\nartifact.size_bytes={}\nartifact.signed={}\nprovenance.builder={}\nprovenance.source_commit={}\nprovenance.build_command={}\nprovenance.build_profile={}\nprovenance.sbom={}\nprovenance.scan_status={}\nsignature.key_id={}\nsignature.algorithm={}\nsignature.value={}\n",
            self.format,
            self.version,
            self.source_tag,
            self.source_commit,
            self.artifact.name,
            self.artifact.target,
            self.artifact.format,
            self.artifact.binary,
            self.artifact.digest,
            self.artifact.size_bytes,
            self.artifact.signed,
            self.provenance.builder,
            self.provenance.source_commit,
            self.provenance.build_command,
            self.provenance.build_profile,
            self.provenance.sbom,
            self.provenance.scan_status,
            self.signature.key_id,
            self.signature.algorithm,
            self.signature.value,
        )
    }

    fn signature_payload(&self) -> String {
        format!(
            "format={}\nversion={}\nsource_tag={}\nsource_commit={}\nartifact.name={}\nartifact.target={}\nartifact.format={}\nartifact.binary={}\nartifact.digest={}\nartifact.size_bytes={}\nartifact.signed={}\nprovenance.builder={}\nprovenance.source_commit={}\nprovenance.build_command={}\nprovenance.build_profile={}\nprovenance.sbom={}\nprovenance.scan_status={}\n",
            self.format,
            self.version,
            self.source_tag,
            self.source_commit,
            self.artifact.name,
            self.artifact.target,
            self.artifact.format,
            self.artifact.binary,
            self.artifact.digest,
            self.artifact.size_bytes,
            self.artifact.signed,
            self.provenance.builder,
            self.provenance.source_commit,
            self.provenance.build_command,
            self.provenance.build_profile,
            self.provenance.sbom,
            self.provenance.scan_status,
        )
    }
}

fn parse_key_value_manifest(data: &str) -> Result<BTreeMap<String, String>, EvaError> {
    let mut fields = BTreeMap::new();
    for line in data.lines() {
        let line = line.trim_start_matches('\u{feff}');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            return Err(EvaError::invalid_argument(
                "release artifact evidence line must use key=value format",
            ));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(EvaError::invalid_argument(
                "release artifact evidence key cannot be empty",
            ));
        }
        if fields
            .insert(key.to_owned(), value.trim().to_owned())
            .is_some()
        {
            return Err(EvaError::invalid_argument(
                "release artifact evidence field is duplicated",
            )
            .with_context("field", key));
        }
    }
    Ok(fields)
}

fn required(fields: &BTreeMap<String, String>, key: &str) -> Result<String, EvaError> {
    fields.get(key).cloned().ok_or_else(|| {
        EvaError::invalid_argument("release artifact evidence is missing required field")
            .with_context("required_field", key)
    })
}

fn parse_bool(value: String, field: &str) -> Result<bool, EvaError> {
    match value.as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(EvaError::invalid_argument(
            "release artifact boolean field must be true or false",
        )
        .with_context("field", field)
        .with_context("value", value)),
    }
}

fn parse_size(value: String) -> Result<u64, EvaError> {
    value.parse::<u64>().map_err(|error| {
        EvaError::invalid_argument("release artifact size must be an integer")
            .with_context("field", "artifact.size_bytes")
            .with_context("value", value)
            .with_context("parse_error", error.to_string())
    })
}

fn validate_version(value: String) -> Result<String, EvaError> {
    let value = validate_non_empty("release version", value)?;
    if value.contains(char::is_whitespace) {
        return Err(
            EvaError::invalid_argument("release version cannot contain whitespace")
                .with_context("version", value),
        );
    }
    Ok(value)
}

fn validate_token(field: &str, value: String) -> Result<String, EvaError> {
    let value = validate_non_empty(field, value)?;
    if value.contains(char::is_whitespace) {
        return Err(
            EvaError::invalid_argument(format!("{field} cannot contain whitespace"))
                .with_context("value", value),
        );
    }
    Ok(value)
}

fn validate_non_empty(field: &str, value: String) -> Result<String, EvaError> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(
            EvaError::invalid_argument(format!("{field} must be non-empty and trimmed"))
                .with_context("value", value),
        );
    }
    Ok(value)
}

fn validate_artifact_name(value: String) -> Result<String, EvaError> {
    let value = validate_token("release artifact name", value)?;
    if value.contains('/') || value.contains('\\') || value.contains("..") {
        return Err(
            EvaError::invalid_argument("release artifact name must be a stable file name")
                .with_context("artifact", value),
        );
    }
    Ok(value)
}

fn validate_digest(value: String) -> Result<String, EvaError> {
    let value = validate_token("release artifact digest", value)?;
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(
            EvaError::invalid_argument("release artifact digest must use sha256 prefix")
                .with_context("digest", value),
        );
    };
    if hex.len() != 64 || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(EvaError::invalid_argument(
            "release artifact digest must be a sha256 hex digest",
        )
        .with_context("digest", value));
    }
    Ok(value)
}

fn validate_commit(value: String) -> Result<String, EvaError> {
    let value = validate_token("release source commit", value)?;
    if value.len() != 40 || !value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(EvaError::invalid_argument(
            "release source commit must be a full 40-character hex sha",
        )
        .with_context("source_commit", value));
    }
    Ok(value)
}

fn keyed_digest(payload: &str, signing_key: &ReleaseArtifactSigningKey) -> String {
    let signed_payload = format!(
        "eva-release-signature:v1\nkey_id={}\nsecret={}\n{}",
        signing_key.key_id, signing_key.secret, payload
    );
    ArtifactRecord::new("release/artifact/signature", signed_payload.into_bytes()).digest
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const DIGEST: &str = "sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df";

    fn signed_evidence() -> ReleaseArtifactEvidence {
        let key = ReleaseArtifactSigningKey::local_development();
        let artifact = ReleaseArtifactSubject::new(
            "eva-cli-1.11.5-alpha-x86_64-unknown-linux-gnu.tar.gz",
            "x86_64-unknown-linux-gnu",
            "tar.gz",
            "eva",
            DIGEST,
            1024,
            true,
        )
        .unwrap();
        let provenance = ReleaseProvenanceEvidence::new(
            "github-actions",
            COMMIT,
            "cargo-build-release-locked-bin-eva",
            "release",
            "spdx:release-evidence/eva.spdx.json",
            "passed",
        )
        .unwrap();
        let signature =
            ReleaseArtifactSignature::new(key.key_id(), RELEASE_SIGNATURE_ALGORITHM, "pending")
                .unwrap();
        let mut evidence = ReleaseArtifactEvidence::new(
            "1.11.5-alpha",
            "v1.11.5-alpha",
            COMMIT,
            artifact,
            provenance,
            signature,
        )
        .unwrap();
        evidence.signature = evidence.sign(&key);
        evidence
    }

    #[test]
    fn signed_artifact_evidence_round_trips_and_verifies() {
        let key = ReleaseArtifactSigningKey::local_development();
        let evidence =
            ReleaseArtifactEvidence::parse_manifest(&signed_evidence().to_manifest()).unwrap();

        let report = evidence.verify(&key);

        assert_eq!(report.status, "verified");
        assert!(report.signature_verified);
        assert!(report.provenance_verified);
        assert!(report.risks.is_empty());
    }

    #[test]
    fn unsigned_artifact_blocks_verification() {
        let key = ReleaseArtifactSigningKey::local_development();
        let mut evidence = signed_evidence();
        evidence.artifact.signed = false;
        evidence.signature = evidence.sign(&key);

        let report = evidence.verify(&key);

        assert_eq!(report.status, "blocked");
        assert!(!report.signature_verified);
        assert!(report
            .risks
            .iter()
            .any(|risk| risk == "release artifact is marked unsigned"));
    }

    #[test]
    fn signature_mismatch_blocks_verification() {
        let key = ReleaseArtifactSigningKey::local_development();
        let mut evidence = signed_evidence();
        evidence.signature.value = "sha256:bad".to_owned();

        let report = evidence.verify(&key);

        assert_eq!(report.status, "blocked");
        assert!(!report.signature_verified);
        assert!(report
            .risks
            .iter()
            .any(|risk| risk == "release artifact signature value mismatch"));
    }

    #[test]
    fn provenance_commit_mismatch_blocks_verification() {
        let key = ReleaseArtifactSigningKey::local_development();
        let mut evidence = signed_evidence();
        evidence.provenance.source_commit = "abcdef0123456789abcdef0123456789abcdef01".to_owned();
        evidence.signature = evidence.sign(&key);

        let report = evidence.verify(&key);

        assert_eq!(report.status, "blocked");
        assert!(report.signature_verified);
        assert!(!report.provenance_verified);
    }
}
