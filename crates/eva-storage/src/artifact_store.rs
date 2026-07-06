//! Artifact store contracts and the V0.4 in-memory implementation.

use eva_core::EvaError;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;

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

impl InMemoryArtifactStore {
    pub fn new() -> Self {
        Self::default()
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
