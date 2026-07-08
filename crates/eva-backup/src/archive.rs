//! Signed and optionally sealed backup archive contracts.

use eva_core::EvaError;
use eva_storage::ArtifactRecord;
use std::fmt;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "signed backup archive and remote target contracts";

const ARCHIVE_FORMAT: &str = "eva.backup.archive.v1";
const SIGNATURE_ALGORITHM: &str = "sha256-keyed-v1";
const ENCRYPTION_ALGORITHM: &str = "xor-sha256-stream-v1";

#[derive(Clone, PartialEq, Eq)]
pub struct BackupSigningKey {
    key_id: String,
    secret: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct BackupEncryptionKey {
    key_id: String,
    secret: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupSignatureManifest {
    pub key_id: String,
    pub algorithm: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupEncryptionManifest {
    pub key_id: String,
    pub algorithm: String,
    pub plaintext_checksum: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteBackupTargetKind {
    FilesystemMirror,
    ObjectStore,
    S3Compatible,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBackupTarget {
    pub kind: RemoteBackupTargetKind,
    pub endpoint: String,
    pub prefix: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupArchiveManifest {
    pub format: String,
    pub artifact_key: String,
    pub checksum: String,
    pub plaintext_checksum: String,
    pub encrypted: bool,
    pub signature: BackupSignatureManifest,
    pub encryption: Option<BackupEncryptionManifest>,
    pub remote_target: Option<RemoteBackupTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedBackupArchive {
    pub bytes: Vec<u8>,
    pub manifest: BackupArchiveManifest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupSignatureVerification {
    pub key_id: String,
    pub algorithm: String,
    pub expected_signature: String,
    pub actual_signature: String,
    pub verified: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BackupArchiveCodec;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BackupArchiveVerifier;

impl BackupSigningKey {
    pub fn new(key_id: impl Into<String>, secret: impl Into<String>) -> Result<Self, EvaError> {
        let key_id = validate_key_id("backup signing key id", key_id.into())?;
        let secret = secret.into();
        if secret.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "backup signing key secret cannot be empty",
            ));
        }
        Ok(Self { key_id, secret })
    }

    pub fn local_development() -> Self {
        Self {
            key_id: "eva-local-dev-signing-key".to_owned(),
            secret: "eva-local-development-backup-signing-secret".to_owned(),
        }
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }
}

impl BackupEncryptionKey {
    pub fn new(key_id: impl Into<String>, secret: impl Into<String>) -> Result<Self, EvaError> {
        let key_id = validate_key_id("backup encryption key id", key_id.into())?;
        let secret = secret.into();
        if secret.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "backup encryption key secret cannot be empty",
            ));
        }
        Ok(Self { key_id, secret })
    }

    pub fn local_development() -> Self {
        Self {
            key_id: "eva-local-dev-encryption-key".to_owned(),
            secret: "eva-local-development-backup-encryption-secret".to_owned(),
        }
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }
}

impl fmt::Debug for BackupSigningKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackupSigningKey")
            .field("key_id", &self.key_id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl fmt::Debug for BackupEncryptionKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackupEncryptionKey")
            .field("key_id", &self.key_id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl RemoteBackupTargetKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FilesystemMirror => "filesystem_mirror",
            Self::ObjectStore => "object_store",
            Self::S3Compatible => "s3_compatible",
        }
    }
}

impl RemoteBackupTarget {
    pub fn new(
        kind: RemoteBackupTargetKind,
        endpoint: impl Into<String>,
        prefix: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let endpoint = endpoint.into();
        let prefix = prefix.into();
        if endpoint.trim().is_empty() || endpoint.trim() != endpoint {
            return Err(EvaError::invalid_argument(
                "remote backup target endpoint must be non-empty and trimmed",
            ));
        }
        if prefix.trim().is_empty()
            || prefix.trim() != prefix
            || prefix.contains("..")
            || prefix.contains('\\')
        {
            return Err(EvaError::invalid_argument(
                "remote backup target prefix must be a stable relative prefix",
            )
            .with_context("prefix", prefix));
        }
        Ok(Self {
            kind,
            endpoint,
            prefix,
            required: false,
        })
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
}

impl BackupArchiveCodec {
    pub fn seal(
        artifact_key: impl Into<String>,
        plaintext: Vec<u8>,
        signing_key: &BackupSigningKey,
        encryption_key: Option<&BackupEncryptionKey>,
        remote_target: Option<RemoteBackupTarget>,
    ) -> SealedBackupArchive {
        let artifact_key = artifact_key.into();
        let plaintext_checksum = digest_bytes(&plaintext);
        let (bytes, encryption) = match encryption_key {
            Some(key) => {
                let sealed = xor_with_key(&plaintext, key);
                (
                    sealed,
                    Some(BackupEncryptionManifest {
                        key_id: key.key_id.clone(),
                        algorithm: ENCRYPTION_ALGORITHM.to_owned(),
                        plaintext_checksum: plaintext_checksum.clone(),
                    }),
                )
            }
            None => (plaintext, None),
        };
        let checksum = digest_bytes(&bytes);
        let signature = sign_archive(
            &artifact_key,
            &checksum,
            &plaintext_checksum,
            encryption.as_ref(),
            remote_target.as_ref(),
            signing_key,
        );
        let manifest = BackupArchiveManifest {
            format: ARCHIVE_FORMAT.to_owned(),
            artifact_key,
            checksum,
            plaintext_checksum,
            encrypted: encryption.is_some(),
            signature,
            encryption,
            remote_target,
        };
        SealedBackupArchive { bytes, manifest }
    }

    pub fn open(
        record: &ArtifactRecord,
        manifest: &BackupArchiveManifest,
        encryption_key: Option<&BackupEncryptionKey>,
    ) -> Result<Vec<u8>, EvaError> {
        verify_record_checksum(record, &manifest.checksum)?;
        if manifest.artifact_key != record.key {
            return Err(EvaError::conflict("backup archive artifact key mismatch")
                .with_context("expected_artifact_key", &manifest.artifact_key)
                .with_context("actual_artifact_key", &record.key));
        }
        let plaintext = match &manifest.encryption {
            Some(encryption) => {
                let key = encryption_key.ok_or_else(|| {
                    EvaError::permission_denied("encrypted backup archive requires decryption key")
                        .with_context("artifact_key", &record.key)
                })?;
                if encryption.key_id != key.key_id {
                    return Err(EvaError::permission_denied(
                        "backup archive decryption key id mismatch",
                    )
                    .with_context("expected_key_id", &encryption.key_id)
                    .with_context("actual_key_id", key.key_id()));
                }
                xor_with_key(&record.bytes, key)
            }
            None => record.bytes.clone(),
        };
        let actual_plaintext_checksum = digest_bytes(&plaintext);
        if actual_plaintext_checksum != manifest.plaintext_checksum {
            return Err(
                EvaError::conflict("backup archive plaintext checksum mismatch")
                    .with_context("artifact_key", &record.key)
                    .with_context("expected_digest", &manifest.plaintext_checksum)
                    .with_context("actual_digest", actual_plaintext_checksum),
            );
        }
        Ok(plaintext)
    }
}

impl BackupArchiveVerifier {
    pub fn verify_signature(
        manifest: &BackupArchiveManifest,
        signing_key: &BackupSigningKey,
    ) -> Result<BackupSignatureVerification, EvaError> {
        if manifest.format != ARCHIVE_FORMAT {
            return Err(EvaError::unsupported("unsupported backup archive format")
                .with_context("format", &manifest.format));
        }
        if manifest.signature.key_id != signing_key.key_id {
            return Err(
                EvaError::permission_denied("backup archive signing key id mismatch")
                    .with_context("expected_key_id", &manifest.signature.key_id)
                    .with_context("actual_key_id", signing_key.key_id()),
            );
        }
        if manifest.signature.algorithm != SIGNATURE_ALGORITHM {
            return Err(
                EvaError::unsupported("unsupported backup archive signature algorithm")
                    .with_context("algorithm", &manifest.signature.algorithm),
            );
        }
        let expected_signature = sign_archive(
            &manifest.artifact_key,
            &manifest.checksum,
            &manifest.plaintext_checksum,
            manifest.encryption.as_ref(),
            manifest.remote_target.as_ref(),
            signing_key,
        )
        .value;
        if expected_signature != manifest.signature.value {
            return Err(EvaError::conflict("backup archive signature mismatch")
                .with_context("artifact_key", &manifest.artifact_key)
                .with_context("expected_signature", expected_signature)
                .with_context("actual_signature", &manifest.signature.value));
        }
        Ok(BackupSignatureVerification {
            key_id: manifest.signature.key_id.clone(),
            algorithm: manifest.signature.algorithm.clone(),
            expected_signature,
            actual_signature: manifest.signature.value.clone(),
            verified: true,
        })
    }
}

pub fn verify_record_checksum(
    record: &ArtifactRecord,
    expected_digest: &str,
) -> Result<(), EvaError> {
    let actual_from_bytes = digest_bytes(&record.bytes);
    if actual_from_bytes != record.digest {
        return Err(EvaError::conflict("artifact bytes digest mismatch")
            .with_context("artifact_key", &record.key)
            .with_context("expected_digest", &record.digest)
            .with_context("actual_digest", actual_from_bytes));
    }
    if record.digest != expected_digest {
        return Err(EvaError::conflict("artifact digest mismatch")
            .with_context("artifact_key", &record.key)
            .with_context("expected_digest", expected_digest)
            .with_context("actual_digest", &record.digest));
    }
    Ok(())
}

pub fn digest_bytes(bytes: &[u8]) -> String {
    ArtifactRecord::new("backup/archive/checksum", bytes.to_vec()).digest
}

fn sign_archive(
    artifact_key: &str,
    checksum: &str,
    plaintext_checksum: &str,
    encryption: Option<&BackupEncryptionManifest>,
    remote_target: Option<&RemoteBackupTarget>,
    signing_key: &BackupSigningKey,
) -> BackupSignatureManifest {
    let mut payload = format!(
        "eva-backup-signature:v1\nartifact_key={artifact_key}\nchecksum={checksum}\nplaintext_checksum={plaintext_checksum}\n"
    );
    match encryption {
        Some(encryption) => {
            payload.push_str(&format!(
                "encryption_algorithm={}\nencryption_key_id={}\n",
                encryption.algorithm, encryption.key_id
            ));
        }
        None => payload.push_str("encryption_algorithm=none\nencryption_key_id=none\n"),
    }
    match remote_target {
        Some(target) => {
            payload.push_str(&format!(
                "remote_kind={}\nremote_endpoint={}\nremote_prefix={}\nremote_required={}\n",
                target.kind.as_str(),
                target.endpoint,
                target.prefix,
                target.required
            ));
        }
        None => payload.push_str(
            "remote_kind=none\nremote_endpoint=none\nremote_prefix=none\nremote_required=false\n",
        ),
    }
    payload.push_str(&format!(
        "key_id={}\nsecret={}\n",
        signing_key.key_id, signing_key.secret
    ));
    BackupSignatureManifest {
        key_id: signing_key.key_id.clone(),
        algorithm: SIGNATURE_ALGORITHM.to_owned(),
        value: digest_bytes(payload.as_bytes()),
    }
}

fn xor_with_key(bytes: &[u8], key: &BackupEncryptionKey) -> Vec<u8> {
    bytes
        .iter()
        .enumerate()
        .map(|(index, byte)| byte ^ stream_byte(key, index))
        .collect()
}

fn stream_byte(key: &BackupEncryptionKey, index: usize) -> u8 {
    let seed = format!(
        "eva-backup-encryption-stream:v1\nkey_id={}\nsecret={}\nblock={}\n",
        key.key_id,
        key.secret,
        index / 71
    );
    let digest = digest_bytes(seed.as_bytes());
    digest.as_bytes()[index % digest.len()]
}

fn validate_key_id(field: &'static str, value: String) -> Result<String, EvaError> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(
            EvaError::invalid_argument("backup key id must be non-empty and trimmed")
                .with_context("field", field)
                .with_context("value", value),
        );
    }
    if value.contains('/') || value.contains('\\') || value.contains("..") {
        return Err(
            EvaError::invalid_argument("backup key id must be a stable slug")
                .with_context("field", field)
                .with_context("value", value),
        );
    }
    Ok(value)
}
