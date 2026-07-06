//! Artifact store contracts and the V0.4 in-memory implementation.

use eva_core::EvaError;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "artifact store interfaces and integrity boundaries";

/// Stored artifact bytes and deterministic SHA-256 digest metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRecord {
    pub key: String,
    pub bytes: Vec<u8>,
    pub digest: String,
}

/// Minimal artifact store behavior retained for V0.4 module completeness.
pub trait ArtifactStore {
    fn put_bytes(
        &mut self,
        key: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<ArtifactRecord, EvaError>;
    fn get_bytes(&self, key: &str) -> Option<ArtifactRecord>;
}

/// In-memory artifact store for tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryArtifactStore {
    records: BTreeMap<String, ArtifactRecord>,
}

/// Filesystem-backed artifact store for durable local evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemArtifactStore {
    root: PathBuf,
}

impl InMemoryArtifactStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl FileSystemArtifactStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn try_get_bytes(&self, key: &str) -> Result<Option<ArtifactRecord>, EvaError> {
        let key = validate_filesystem_artifact_key(key.to_owned())?;
        let artifact_path = keyed_path(&self.root.join("objects"), &key, "artifact");
        let metadata_path = keyed_path(&self.root.join("metadata"), &key, "metadata");

        let bytes = match fs::read(&artifact_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(filesystem_error(
                    "failed to read artifact bytes",
                    &key,
                    &artifact_path,
                    error,
                ));
            }
        };

        let metadata = read_metadata(&metadata_path, &key)?;
        let actual_digest = sha256_digest(&bytes);
        if metadata.key != key {
            return Err(EvaError::conflict("artifact metadata key mismatch")
                .with_context("artifact_key", key)
                .with_context("metadata_key", metadata.key));
        }
        if metadata.size_bytes != bytes.len() {
            return Err(EvaError::conflict("artifact size mismatch")
                .with_context("artifact_key", key)
                .with_context("expected_size", metadata.size_bytes.to_string())
                .with_context("actual_size", bytes.len().to_string()));
        }
        if metadata.digest != actual_digest {
            return Err(EvaError::conflict("artifact digest mismatch")
                .with_context("artifact_key", key)
                .with_context("expected_digest", metadata.digest)
                .with_context("actual_digest", actual_digest));
        }

        Ok(Some(ArtifactRecord {
            key,
            bytes,
            digest: actual_digest,
        }))
    }
}

impl ArtifactStore for InMemoryArtifactStore {
    fn put_bytes(
        &mut self,
        key: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<ArtifactRecord, EvaError> {
        let key = key.into();
        if key.trim().is_empty() {
            return Err(EvaError::invalid_argument("artifact key cannot be empty"));
        }
        let bytes = bytes.into();
        let record = ArtifactRecord {
            key: key.clone(),
            digest: sha256_digest(&bytes),
            bytes,
        };
        self.records.insert(key, record.clone());
        Ok(record)
    }

    fn get_bytes(&self, key: &str) -> Option<ArtifactRecord> {
        self.records.get(key).cloned()
    }
}

impl ArtifactStore for FileSystemArtifactStore {
    fn put_bytes(
        &mut self,
        key: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<ArtifactRecord, EvaError> {
        let key = validate_filesystem_artifact_key(key.into())?;
        let bytes = bytes.into();
        let digest = sha256_digest(&bytes);
        let artifact_path = keyed_path(&self.root.join("objects"), &key, "artifact");
        let metadata_path = keyed_path(&self.root.join("metadata"), &key, "metadata");

        if let Some(parent) = artifact_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                filesystem_error("failed to create artifact directory", &key, parent, error)
            })?;
        }
        if let Some(parent) = metadata_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                filesystem_error(
                    "failed to create artifact metadata directory",
                    &key,
                    parent,
                    error,
                )
            })?;
        }

        fs::write(&artifact_path, &bytes).map_err(|error| {
            filesystem_error(
                "failed to write artifact bytes",
                &key,
                &artifact_path,
                error,
            )
        })?;
        fs::write(&metadata_path, metadata_payload(&key, &digest, bytes.len())).map_err(
            |error| {
                filesystem_error(
                    "failed to write artifact metadata",
                    &key,
                    &metadata_path,
                    error,
                )
            },
        )?;

        Ok(ArtifactRecord { key, bytes, digest })
    }

    fn get_bytes(&self, key: &str) -> Option<ArtifactRecord> {
        self.try_get_bytes(key).ok().flatten()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactMetadata {
    key: String,
    digest: String,
    size_bytes: usize,
}

fn sha256_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();

    let mut encoded = String::with_capacity("sha256:".len() + digest.len() * 2);
    encoded.push_str("sha256:");
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    encoded
}

fn validate_filesystem_artifact_key(key: String) -> Result<String, EvaError> {
    if key.trim().is_empty() || key.trim() != key || key.contains('\\') {
        return Err(
            EvaError::invalid_argument("artifact key must be a stable relative path")
                .with_context("artifact_key", key),
        );
    }
    for segment in key.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(
                EvaError::invalid_argument("artifact key must be a stable relative path")
                    .with_context("artifact_key", key),
            );
        }
        if !segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(EvaError::invalid_argument(
                "artifact key contains unsupported filesystem characters",
            )
            .with_context("artifact_key", key));
        }
    }
    Ok(key)
}

fn keyed_path(root: &Path, key: &str, extension: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    let mut segments = key.split('/').peekable();
    while let Some(segment) = segments.next() {
        if segments.peek().is_some() {
            path.push(segment);
        } else {
            path.push(format!("{segment}.{extension}"));
        }
    }
    path
}

fn metadata_payload(key: &str, digest: &str, size_bytes: usize) -> String {
    format!("key={key}\ndigest={digest}\nsize_bytes={size_bytes}\n")
}

fn read_metadata(path: &Path, key: &str) -> Result<ArtifactMetadata, EvaError> {
    let data = fs::read_to_string(path).map_err(|error| {
        let message = if error.kind() == std::io::ErrorKind::NotFound {
            "artifact metadata is missing"
        } else {
            "failed to read artifact metadata"
        };
        filesystem_error(message, key, path, error)
    })?;
    parse_metadata(&data).map_err(|error| {
        error
            .with_context("artifact_key", key)
            .with_context("path", path.display().to_string())
    })
}

fn parse_metadata(data: &str) -> Result<ArtifactMetadata, EvaError> {
    let mut key = None;
    let mut digest = None;
    let mut size_bytes = None;
    for line in data.lines() {
        let Some((name, value)) = line.split_once('=') else {
            return Err(EvaError::conflict("artifact metadata is invalid"));
        };
        match name {
            "key" => key = Some(value.to_owned()),
            "digest" => digest = Some(value.to_owned()),
            "size_bytes" => {
                size_bytes = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| EvaError::conflict("artifact metadata is invalid"))?,
                );
            }
            _ => return Err(EvaError::conflict("artifact metadata is invalid")),
        }
    }
    Ok(ArtifactMetadata {
        key: key.ok_or_else(|| EvaError::conflict("artifact metadata is invalid"))?,
        digest: digest.ok_or_else(|| EvaError::conflict("artifact metadata is invalid"))?,
        size_bytes: size_bytes.ok_or_else(|| EvaError::conflict("artifact metadata is invalid"))?,
    })
}

fn filesystem_error(message: &str, key: &str, path: &Path, error: std::io::Error) -> EvaError {
    EvaError::internal(message)
        .with_context("artifact_key", key)
        .with_context("path", path.display().to_string())
        .with_context("io_error", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn artifact_round_trip_preserves_digest() {
        let mut store = InMemoryArtifactStore::new();

        let record = store.put_bytes("trace/basic", b"ok".as_slice()).unwrap();
        let loaded = store.get_bytes("trace/basic").unwrap();

        assert_eq!(
            record.digest,
            "sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df"
        );
        assert_eq!(loaded.bytes, b"ok");
    }

    #[test]
    fn filesystem_artifact_store_round_trips_bytes_and_digest() {
        let root = test_root("round-trip");
        let mut store = FileSystemArtifactStore::new(root.path());

        let record = store.put_bytes("backup/basic", b"ok".as_slice()).unwrap();
        let loaded = store.get_bytes("backup/basic").unwrap();

        assert_eq!(store.root(), root.path());
        assert_eq!(loaded, record);
        assert_eq!(loaded.bytes, b"ok");
        assert!(loaded.digest.starts_with("sha256:"));
    }

    #[test]
    fn filesystem_artifact_store_reports_digest_mismatch() {
        let root = test_root("digest-mismatch");
        let mut store = FileSystemArtifactStore::new(root.path());
        store.put_bytes("backup/tamper", b"ok".as_slice()).unwrap();
        let artifact_path = keyed_path(&root.path().join("objects"), "backup/tamper", "artifact");
        fs::write(artifact_path, b"changed").unwrap();

        let error = store.try_get_bytes("backup/tamper").unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    fn filesystem_artifact_store_returns_none_for_missing_artifacts() {
        let root = test_root("missing");
        let store = FileSystemArtifactStore::new(root.path());

        assert!(store.get_bytes("backup/missing").is_none());
        assert!(store.try_get_bytes("backup/missing").unwrap().is_none());
    }

    #[test]
    fn filesystem_artifact_store_rejects_unsafe_keys() {
        let root = test_root("unsafe-key");
        let mut store = FileSystemArtifactStore::new(root.path());

        let error = store
            .put_bytes("../escape", b"nope".as_slice())
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
    }

    struct TestRoot {
        path: PathBuf,
    }

    impl TestRoot {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn test_root(name: &str) -> TestRoot {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        TestRoot {
            path: std::env::temp_dir()
                .join(format!("eva-storage-{name}-{}-{now}", std::process::id())),
        }
    }
}
