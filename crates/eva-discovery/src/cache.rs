//! In-memory discovery result cache.

use crate::normalizer::{dedupe, DiscoveryCandidate};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "cache discovery results without granting runtime handles";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiscoveryCache {
    snapshot: Vec<DiscoveryCandidate>,
    refresh_reason: Option<String>,
}

impl DiscoveryCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn replace(&mut self, snapshot: Vec<DiscoveryCandidate>, reason: impl Into<String>) {
        self.snapshot = snapshot;
        self.refresh_reason = Some(reason.into());
    }

    pub fn merge_source(
        &mut self,
        source_id: &str,
        candidates: Vec<DiscoveryCandidate>,
        reason: impl Into<String>,
    ) {
        self.snapshot
            .retain(|candidate| candidate.source.as_str() != source_id);
        self.snapshot.extend(candidates);
        self.snapshot = dedupe(std::mem::take(&mut self.snapshot));
        self.refresh_reason = Some(reason.into());
    }

    pub fn snapshot(&self) -> &[DiscoveryCandidate] {
        &self.snapshot
    }

    pub fn refresh_reason(&self) -> Option<&str> {
        self.refresh_reason.as_deref()
    }
}
