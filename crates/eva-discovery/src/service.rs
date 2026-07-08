//! Discovery service coordination.

use crate::cache::DiscoveryCache;
use crate::health::{DiscoveryHealth, DiscoveryHealthStatus};
use crate::normalizer::DiscoveryCandidate;
use crate::scanner::{scan_sources, DiscoveryScanReport, DiscoverySource, ProjectDiscoverySource};
use crate::sources::codex::CodexDiscoverySource;
use crate::sources::mcp::McpDiscoverySource;
use crate::sources::omx::OmxDiscoverySource;
use crate::sources::path_commands::PathCommandDiscoverySource;
use crate::sources::registry::ExternalRegistryDiscoverySource;
use eva_config::ProjectConfig;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "coordinate trusted source discovery without authorization";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiscoveryService {
    cache: DiscoveryCache,
}

impl DiscoveryService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scan_project(&mut self, project: &ProjectConfig) -> DiscoveryScanReport {
        let report = scan_project_sources(project);
        self.cache
            .replace(report.candidates.clone(), "project multi-source scan");
        report
    }

    pub fn scan_project_incremental(&mut self, project: &ProjectConfig) -> DiscoveryScanReport {
        let mut report = scan_project_sources(project);
        for source_report in &report.source_reports {
            if source_report.error.is_none() {
                self.cache.merge_source(
                    &source_report.cache_key,
                    source_report.candidates.clone(),
                    format!("incremental scan: {}", source_report.cache_key),
                );
            }
        }
        report.candidates = self.cache.snapshot().to_vec();
        report
    }

    pub fn cache(&self) -> &DiscoveryCache {
        &self.cache
    }

    pub fn candidates(&self) -> &[DiscoveryCandidate] {
        self.cache.snapshot()
    }

    pub fn health(&self) -> Vec<DiscoveryHealth> {
        self.cache
            .snapshot()
            .iter()
            .map(|candidate| DiscoveryHealth {
                candidate_id: candidate.id.clone(),
                status: if candidate.rejected_reason.is_some() {
                    DiscoveryHealthStatus::Rejected
                } else {
                    DiscoveryHealthStatus::Seen
                },
                message: candidate.rejected_reason.clone().unwrap_or_else(|| {
                    "candidate discovered; authorization remains external".to_owned()
                }),
            })
            .collect()
    }
}

fn scan_project_sources(project: &ProjectConfig) -> DiscoveryScanReport {
    let project_source = ProjectDiscoverySource::new(project);
    let path_source = PathCommandDiscoverySource::new(project);
    let mcp_source = McpDiscoverySource::new(project);
    let omx_source = OmxDiscoverySource::new(project);
    let codex_source = CodexDiscoverySource::new(project);
    let registry_source = ExternalRegistryDiscoverySource::new(project);
    scan_sources(&[
        &project_source as &dyn DiscoverySource,
        &path_source as &dyn DiscoverySource,
        &mcp_source as &dyn DiscoverySource,
        &omx_source as &dyn DiscoverySource,
        &codex_source as &dyn DiscoverySource,
        &registry_source as &dyn DiscoverySource,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use std::path::{Path, PathBuf};

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    fn service_caches_candidates_without_handles() {
        let project = load_project_config(workspace_root()).unwrap();
        let mut service = DiscoveryService::new();

        let report = service.scan_project(&project);

        assert_eq!(service.candidates().len(), report.candidates.len());
        assert!(service
            .candidates()
            .iter()
            .all(|candidate| !candidate.handle_granted));
        assert!(report
            .source_reports
            .iter()
            .any(|source| source.source_id == "path_commands"));
        assert!(report
            .source_reports
            .iter()
            .any(|source| source.source_id == "mcp"));
        assert!(report
            .source_reports
            .iter()
            .any(|source| source.source_id == "codex"));
    }

    #[test]
    fn incremental_scan_updates_sources_without_granting_handles() {
        let project = load_project_config(workspace_root()).unwrap();
        let mut service = DiscoveryService::new();

        let first = service.scan_project_incremental(&project);
        let second = service.scan_project_incremental(&project);

        assert_eq!(service.candidates().len(), second.candidates.len());
        assert_eq!(first.source_reports.len(), second.source_reports.len());
        assert!(service
            .cache()
            .refresh_reason()
            .unwrap()
            .starts_with("incremental scan:"));
        assert!(service
            .candidates()
            .iter()
            .all(|candidate| !candidate.handle_granted));
    }

    #[test]
    fn rejected_source_candidates_are_visible_in_health() {
        let project = load_project_config(workspace_root()).unwrap();
        let mut service = DiscoveryService::new();

        service.scan_project_incremental(&project);
        let health = service.health();

        assert!(health.iter().any(|entry| {
            entry.status == DiscoveryHealthStatus::Rejected
                && entry
                    .message
                    .contains("external registry source is not configured")
        }));
    }
}
