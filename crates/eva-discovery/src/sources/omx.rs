//! OMX workflow discovery from local workspace state.

use crate::normalizer::{DiscoveryCandidate, DiscoveryCandidateKind, DiscoveryTrust};
use crate::scanner::DiscoverySource;
use eva_config::ProjectConfig;
use eva_core::EvaError;

pub const RESPONSIBILITY: &str = "discover trusted OMX workflow surfaces";

pub struct OmxDiscoverySource<'a> {
    project: &'a ProjectConfig,
}

impl<'a> OmxDiscoverySource<'a> {
    pub fn new(project: &'a ProjectConfig) -> Self {
        Self { project }
    }
}

impl DiscoverySource for OmxDiscoverySource<'_> {
    fn source_id(&self) -> &str {
        "omx"
    }

    fn timeout_ms(&self) -> u64 {
        100
    }

    fn scan(&self) -> Result<Vec<DiscoveryCandidate>, EvaError> {
        let omx_root = self.project.project_root.join(".omx");
        let trust = if omx_root.is_dir() {
            DiscoveryTrust::ConfiguredAllowlist
        } else {
            DiscoveryTrust::DisplayOnly
        };
        let mut candidate = DiscoveryCandidate::named(
            self.source_id(),
            DiscoveryCandidateKind::Workflow,
            "omx-workspace",
            None,
            trust,
        );
        if !omx_root.is_dir() {
            candidate = candidate.rejected(".omx state directory is not present");
        }
        Ok(vec![candidate])
    }
}
