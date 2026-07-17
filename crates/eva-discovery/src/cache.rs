//! Per-source discovery cache with optional durable persistence and TTL enforcement.

use crate::normalizer::{dedupe, DiscoveryCandidate, DiscoveryCandidateKind, DiscoveryTrust};
use eva_core::{sha256_digest, AdapterId, CapabilityName, EvaError};
use eva_storage::{atomic_write, DurableBackendLayout};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const RESPONSIBILITY: &str =
    "cache discovery results per source without granting runtime handles";
const FORMAT: &str = "eva.discovery-cache.v1";
pub const DEFAULT_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryCachePartition {
    pub source_id: String,
    pub fetched_at_ms: u128,
    pub expires_at_ms: u128,
    pub source_digest: String,
    pub candidates: Vec<DiscoveryCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryCache {
    partitions: BTreeMap<String, DiscoveryCachePartition>,
    snapshot: Vec<DiscoveryCandidate>,
    refresh_reason: Option<String>,
    directory: Option<PathBuf>,
    ttl: Duration,
}

impl Default for DiscoveryCache {
    fn default() -> Self {
        Self {
            partitions: BTreeMap::new(),
            snapshot: Vec::new(),
            refresh_reason: None,
            directory: None,
            ttl: DEFAULT_TTL,
        }
    }
}

impl DiscoveryCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(layout: &DurableBackendLayout, ttl: Duration) -> Result<Self, EvaError> {
        let directory = layout.state_dir.join("discovery-cache");
        fs::create_dir_all(&directory)
            .map_err(|_| EvaError::internal("create durable discovery cache directory"))?;
        Self::open_directory(directory, ttl, now_ms())
    }

    fn open_directory(directory: PathBuf, ttl: Duration, now: u128) -> Result<Self, EvaError> {
        let mut cache = Self {
            directory: Some(directory.clone()),
            ttl,
            ..Self::default()
        };
        let entries = fs::read_dir(&directory)
            .map_err(|_| EvaError::internal("read durable discovery cache directory"))?;
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("cache") {
                continue;
            }
            let partition = fs::read_to_string(&path)
                .ok()
                .and_then(|value| decode_partition(&value).ok());
            match partition {
                Some(partition) if partition.expires_at_ms > now => {
                    cache
                        .partitions
                        .insert(partition.source_id.clone(), partition);
                }
                Some(_) => {
                    let _ = fs::remove_file(path);
                }
                None => {
                    // A corrupt source partition is ignored without affecting healthy sources.
                }
            }
        }
        cache.rebuild_snapshot();
        Ok(cache)
    }

    pub fn replace(&mut self, snapshot: Vec<DiscoveryCandidate>, reason: impl Into<String>) {
        self.partitions.clear();
        let mut by_source: BTreeMap<String, Vec<DiscoveryCandidate>> = BTreeMap::new();
        for candidate in snapshot {
            by_source
                .entry(candidate.source.clone())
                .or_default()
                .push(candidate);
        }
        for (source, candidates) in by_source {
            let _ = self.merge_source(&source, candidates, "full scan");
        }
        self.refresh_reason = Some(reason.into());
    }

    pub fn merge_source(
        &mut self,
        source_id: &str,
        candidates: Vec<DiscoveryCandidate>,
        reason: impl Into<String>,
    ) -> Result<(), EvaError> {
        self.merge_source_at(source_id, candidates, reason, now_ms())
    }

    fn merge_source_at(
        &mut self,
        source_id: &str,
        candidates: Vec<DiscoveryCandidate>,
        reason: impl Into<String>,
        fetched_at_ms: u128,
    ) -> Result<(), EvaError> {
        let candidates = dedupe(candidates);
        if candidates
            .iter()
            .any(|candidate| candidate.source != source_id)
        {
            return Err(EvaError::conflict(
                "discovery cache partition contains a different source",
            ));
        }
        let expires_at_ms = fetched_at_ms.saturating_add(self.ttl.as_millis());
        let source_digest = digest_candidates(&candidates);
        let partition = DiscoveryCachePartition {
            source_id: source_id.to_owned(),
            fetched_at_ms,
            expires_at_ms,
            source_digest,
            candidates,
        };
        if let Some(directory) = &self.directory {
            let path = partition_path(directory, source_id);
            atomic_write(&path, encode_partition(&partition).as_bytes())
                .map_err(|_| EvaError::internal("persist discovery cache partition"))?;
        }
        self.partitions.insert(source_id.to_owned(), partition);
        self.rebuild_snapshot();
        self.refresh_reason = Some(reason.into());
        Ok(())
    }

    pub fn snapshot(&self) -> &[DiscoveryCandidate] {
        &self.snapshot
    }

    pub fn partition(&self, source_id: &str) -> Option<&DiscoveryCachePartition> {
        self.partitions.get(source_id)
    }

    pub fn refresh_reason(&self) -> Option<&str> {
        self.refresh_reason.as_deref()
    }

    fn rebuild_snapshot(&mut self) {
        self.snapshot = dedupe(
            self.partitions
                .values()
                .flat_map(|partition| partition.candidates.iter().cloned())
                .collect(),
        );
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn partition_path(directory: &Path, source_id: &str) -> PathBuf {
    let digest = sha256_digest(source_id.as_bytes());
    directory.join(format!("{}.cache", digest.trim_start_matches("sha256:")))
}

fn digest_candidates(candidates: &[DiscoveryCandidate]) -> String {
    let mut encoded = String::new();
    for candidate in candidates {
        encoded.push_str(&encode_candidate(candidate));
    }
    sha256_digest(encoded.as_bytes())
}

fn encode_partition(partition: &DiscoveryCachePartition) -> String {
    let mut output = format!(
        "format={FORMAT}\nsource={}\nfetched={}\nexpires={}\ndigest={}\ncount={}\n",
        hex(partition.source_id.as_bytes()),
        partition.fetched_at_ms,
        partition.expires_at_ms,
        partition.source_digest,
        partition.candidates.len()
    );
    for candidate in &partition.candidates {
        output.push_str("candidate=");
        output.push_str(&hex(encode_candidate(candidate).as_bytes()));
        output.push('\n');
    }
    output
}

fn decode_partition(value: &str) -> Result<DiscoveryCachePartition, EvaError> {
    let mut format = None;
    let mut source = None;
    let mut fetched = None;
    let mut expires = None;
    let mut digest = None;
    let mut count = None;
    let mut candidates = Vec::new();
    for line in value.lines() {
        let (key, raw) = line
            .split_once('=')
            .ok_or_else(|| EvaError::conflict("invalid discovery cache record"))?;
        match key {
            "format" if format.replace(raw).is_none() => {}
            "source" if source.is_none() => source = Some(unhex_string(raw)?),
            "fetched" if fetched.is_none() => fetched = raw.parse().ok(),
            "expires" if expires.is_none() => expires = raw.parse().ok(),
            "digest" if digest.is_none() => digest = Some(raw.to_owned()),
            "count" if count.is_none() => count = raw.parse::<usize>().ok(),
            "candidate" => candidates.push(decode_candidate(&unhex_string(raw)?)?),
            _ => {
                return Err(EvaError::conflict(
                    "duplicate or unknown discovery cache field",
                ))
            }
        }
    }
    if format != Some(FORMAT) || count != Some(candidates.len()) {
        return Err(EvaError::conflict("unsupported discovery cache record"));
    }
    let source_id = source.ok_or_else(|| EvaError::conflict("missing discovery cache source"))?;
    if candidates
        .iter()
        .any(|candidate| candidate.source != source_id)
    {
        return Err(EvaError::conflict("discovery cache source mismatch"));
    }
    let source_digest =
        digest.ok_or_else(|| EvaError::conflict("missing discovery cache digest"))?;
    if source_digest != digest_candidates(&candidates) {
        return Err(EvaError::conflict("discovery cache digest mismatch"));
    }
    Ok(DiscoveryCachePartition {
        source_id,
        fetched_at_ms: fetched
            .ok_or_else(|| EvaError::conflict("missing discovery cache fetch time"))?,
        expires_at_ms: expires
            .ok_or_else(|| EvaError::conflict("missing discovery cache expiry"))?,
        source_digest,
        candidates,
    })
}

fn encode_candidate(candidate: &DiscoveryCandidate) -> String {
    let shadows = candidate.shadowed_path_digests.join("\u{1f}");
    [
        candidate.id.as_str(),
        candidate.kind.as_str(),
        candidate.source.as_str(),
        candidate.trust.as_str(),
        candidate.adapter_id.as_ref().map_or("", |v| v.as_str()),
        candidate.capability.as_ref().map_or("", |v| v.as_str()),
        if candidate.handle_granted { "1" } else { "0" },
        candidate.rejected_reason.as_deref().unwrap_or(""),
        candidate.resolved_path_digest.as_deref().unwrap_or(""),
        candidate.version.as_deref().unwrap_or(""),
        shadows.as_str(),
    ]
    .iter()
    .map(|field| hex(field.as_bytes()))
    .collect::<Vec<_>>()
    .join(":")
}

fn decode_candidate(value: &str) -> Result<DiscoveryCandidate, EvaError> {
    let fields = value
        .split(':')
        .map(unhex_string)
        .collect::<Result<Vec<_>, _>>()?;
    if fields.len() != 11 || fields[6] != "0" {
        return Err(EvaError::conflict("invalid discovery cache candidate"));
    }
    let kind = match fields[1].as_str() {
        "agent" => DiscoveryCandidateKind::Agent,
        "adapter" => DiscoveryCandidateKind::Adapter,
        "capability" => DiscoveryCandidateKind::Capability,
        "path_command" => DiscoveryCandidateKind::PathCommand,
        "mcp_tool" => DiscoveryCandidateKind::McpTool,
        "skill" => DiscoveryCandidateKind::Skill,
        "workflow" => DiscoveryCandidateKind::Workflow,
        "registry_entry" => DiscoveryCandidateKind::RegistryEntry,
        _ => return Err(EvaError::conflict("invalid discovery cache candidate kind")),
    };
    let trust = match fields[3].as_str() {
        "project_manifest" => DiscoveryTrust::ProjectManifest,
        "configured_allowlist" => DiscoveryTrust::ConfiguredAllowlist,
        "display_only" => DiscoveryTrust::DisplayOnly,
        _ => return Err(EvaError::conflict("invalid discovery cache trust")),
    };
    Ok(DiscoveryCandidate {
        id: fields[0].clone(),
        kind,
        source: fields[2].clone(),
        trust,
        adapter_id: optional(&fields[4]).map(AdapterId::parse).transpose()?,
        capability: optional(&fields[5])
            .map(CapabilityName::parse)
            .transpose()?,
        handle_granted: false,
        rejected_reason: optional(&fields[7]).map(str::to_owned),
        resolved_path_digest: optional(&fields[8]).map(str::to_owned),
        version: optional(&fields[9]).map(str::to_owned),
        shadowed_path_digests: if fields[10].is_empty() {
            Vec::new()
        } else {
            fields[10].split('\u{1f}').map(str::to_owned).collect()
        },
    })
}

fn optional(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn unhex_string(value: &str) -> Result<String, EvaError> {
    if !value.len().is_multiple_of(2) {
        return Err(EvaError::conflict("invalid discovery cache hex"));
    }
    let bytes = value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair)
                .map_err(|_| EvaError::conflict("invalid discovery cache hex"))?;
            u8::from_str_radix(text, 16)
                .map_err(|_| EvaError::conflict("invalid discovery cache hex"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    String::from_utf8(bytes).map_err(|_| EvaError::conflict("invalid discovery cache utf-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn root(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "eva-discovery-cache-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }
    fn candidate(source: &str, name: &str) -> DiscoveryCandidate {
        DiscoveryCandidate::named(
            source,
            DiscoveryCandidateKind::Workflow,
            name,
            None,
            DiscoveryTrust::DisplayOnly,
        )
    }

    #[test]
    fn restart_restores_unexpired_partition_with_metadata() {
        let dir = root("restart");
        fs::create_dir_all(&dir).unwrap();
        let mut cache =
            DiscoveryCache::open_directory(dir.clone(), Duration::from_secs(60), 100).unwrap();
        cache
            .merge_source_at("alpha", vec![candidate("alpha", "one")], "scan", 100)
            .unwrap();
        let reopened =
            DiscoveryCache::open_directory(dir.clone(), Duration::from_secs(60), 101).unwrap();
        let partition = reopened.partition("alpha").unwrap();
        assert_eq!(partition.fetched_at_ms, 100);
        assert_eq!(partition.expires_at_ms, 60_100);
        assert!(partition.source_digest.starts_with("sha256:"));
        assert_eq!(reopened.snapshot().len(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn expired_partition_is_never_returned() {
        let dir = root("expired");
        fs::create_dir_all(&dir).unwrap();
        let mut cache =
            DiscoveryCache::open_directory(dir.clone(), Duration::from_millis(5), 100).unwrap();
        cache
            .merge_source_at("alpha", vec![candidate("alpha", "one")], "scan", 100)
            .unwrap();
        let reopened =
            DiscoveryCache::open_directory(dir.clone(), Duration::from_millis(5), 106).unwrap();
        assert!(reopened.snapshot().is_empty());
        assert!(reopened.partition("alpha").is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_partition_does_not_clear_other_sources() {
        let dir = root("corrupt");
        fs::create_dir_all(&dir).unwrap();
        let mut cache =
            DiscoveryCache::open_directory(dir.clone(), Duration::from_secs(60), 100).unwrap();
        cache
            .merge_source_at("alpha", vec![candidate("alpha", "one")], "scan", 100)
            .unwrap();
        cache
            .merge_source_at("beta", vec![candidate("beta", "two")], "scan", 100)
            .unwrap();
        fs::write(partition_path(&dir, "alpha"), "corrupt").unwrap();
        let reopened =
            DiscoveryCache::open_directory(dir.clone(), Duration::from_secs(60), 101).unwrap();
        assert!(reopened.partition("alpha").is_none());
        assert!(reopened.partition("beta").is_some());
        assert_eq!(reopened.snapshot().len(), 1);
        let _ = fs::remove_dir_all(dir);
    }
}
