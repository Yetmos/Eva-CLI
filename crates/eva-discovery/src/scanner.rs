//! Discovery source orchestration.

use crate::normalizer::{dedupe, DiscoveryCandidate, DiscoveryCandidateKind, DiscoveryTrust};
use eva_config::{AdapterTransport, CapabilityKind, ProjectConfig};
use eva_core::EvaError;
use std::collections::BTreeSet;
use std::time::Instant;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "scan trusted sources for candidate capabilities";

pub trait DiscoverySource {
    fn source_id(&self) -> &str;
    fn cache_key(&self) -> String {
        self.source_id().to_owned()
    }
    fn timeout_ms(&self) -> u64 {
        1_000
    }
    fn scan(&self) -> Result<Vec<DiscoveryCandidate>, EvaError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverySourceReport {
    pub source_id: String,
    pub cache_key: String,
    pub timeout_ms: u64,
    pub elapsed_ms: u64,
    pub status: String,
    pub candidates: Vec<DiscoveryCandidate>,
    pub error: Option<String>,
    pub rejected_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryScanReport {
    pub candidates: Vec<DiscoveryCandidate>,
    pub source_reports: Vec<DiscoverySourceReport>,
}

pub struct ProjectDiscoverySource<'a> {
    project: &'a ProjectConfig,
}

impl<'a> ProjectDiscoverySource<'a> {
    pub fn new(project: &'a ProjectConfig) -> Self {
        Self { project }
    }
}

impl DiscoverySource for ProjectDiscoverySource<'_> {
    fn source_id(&self) -> &str {
        "project_config"
    }

    fn scan(&self) -> Result<Vec<DiscoveryCandidate>, EvaError> {
        let mut candidates = Vec::new();
        for adapter in &self.project.adapters {
            candidates.push(DiscoveryCandidate::adapter(
                self.source_id(),
                adapter.id.clone(),
            ));
            for capability in &adapter.capabilities {
                let kind = match adapter.transport {
                    AdapterTransport::Mcp => DiscoveryCandidateKind::McpTool,
                    AdapterTransport::Skill => DiscoveryCandidateKind::Skill,
                    _ => DiscoveryCandidateKind::Capability,
                };
                let trust = if adapter.enabled {
                    DiscoveryTrust::ProjectManifest
                } else {
                    DiscoveryTrust::DisplayOnly
                };
                let candidate = DiscoveryCandidate::capability(
                    self.source_id(),
                    capability.clone(),
                    Some(adapter.id.clone()),
                    kind,
                    trust,
                );
                candidates.push(if adapter.enabled {
                    candidate
                } else {
                    candidate.rejected("adapter manifest is disabled")
                });
            }
        }
        for capability in &self.project.capabilities {
            let kind = match capability.kind {
                CapabilityKind::McpTool => DiscoveryCandidateKind::McpTool,
                CapabilityKind::Skill => DiscoveryCandidateKind::Skill,
                _ => DiscoveryCandidateKind::Capability,
            };
            candidates.push(DiscoveryCandidate::capability(
                self.source_id(),
                capability.capability.clone(),
                capability
                    .default_provider
                    .clone()
                    .or_else(|| capability.provider.clone()),
                kind,
                DiscoveryTrust::ConfiguredAllowlist,
            ));
        }
        Ok(candidates)
    }
}

pub fn scan_sources(sources: &[&dyn DiscoverySource]) -> DiscoveryScanReport {
    let mut all = Vec::new();
    let mut source_reports = Vec::new();
    for source in sources {
        let cache_key = source.cache_key();
        let timeout_ms = source.timeout_ms();
        if timeout_ms == 0 {
            source_reports.push(DiscoverySourceReport {
                source_id: source.source_id().to_owned(),
                cache_key,
                timeout_ms,
                elapsed_ms: 0,
                status: "timeout".to_owned(),
                candidates: Vec::new(),
                error: Some("discovery source timed out before scan".to_owned()),
                rejected_reason: Some("timeout_ms is zero".to_owned()),
            });
            continue;
        }
        let started = Instant::now();
        match source.scan() {
            Ok(candidates) => {
                let elapsed_ms = elapsed_ms(started);
                if elapsed_ms > timeout_ms {
                    source_reports.push(DiscoverySourceReport {
                        source_id: source.source_id().to_owned(),
                        cache_key,
                        timeout_ms,
                        elapsed_ms,
                        status: "timeout".to_owned(),
                        candidates: Vec::new(),
                        error: Some("discovery source timed out".to_owned()),
                        rejected_reason: Some(format!(
                            "elapsed_ms {elapsed_ms} exceeded timeout_ms {timeout_ms}"
                        )),
                    });
                    continue;
                }
                let status = source_status(&candidates).to_owned();
                let rejected_reason = source_rejected_reason(&candidates);
                all.extend(candidates.clone());
                source_reports.push(DiscoverySourceReport {
                    source_id: source.source_id().to_owned(),
                    cache_key,
                    timeout_ms,
                    elapsed_ms,
                    status,
                    candidates,
                    error: None,
                    rejected_reason,
                });
            }
            Err(error) => source_reports.push(DiscoverySourceReport {
                source_id: source.source_id().to_owned(),
                cache_key,
                timeout_ms,
                elapsed_ms: elapsed_ms(started),
                status: "error".to_owned(),
                candidates: Vec::new(),
                error: Some(error.message().to_owned()),
                rejected_reason: Some(error.message().to_owned()),
            }),
        }
    }
    DiscoveryScanReport {
        candidates: dedupe(all),
        source_reports,
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

fn source_status(candidates: &[DiscoveryCandidate]) -> &'static str {
    if !candidates.is_empty()
        && candidates
            .iter()
            .all(|candidate| candidate.rejected_reason.is_some())
    {
        "rejected"
    } else {
        "ok"
    }
}

fn source_rejected_reason(candidates: &[DiscoveryCandidate]) -> Option<String> {
    let reasons = candidates
        .iter()
        .filter_map(|candidate| candidate.rejected_reason.as_deref())
        .collect::<BTreeSet<_>>();
    if reasons.is_empty() {
        None
    } else {
        Some(reasons.into_iter().collect::<Vec<_>>().join("; "))
    }
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
    fn project_source_finds_mcp_and_skill_candidates() {
        let project = load_project_config(workspace_root()).unwrap();
        let source = ProjectDiscoverySource::new(&project);
        let candidates = source.scan().unwrap();

        assert!(candidates
            .iter()
            .any(|candidate| candidate.kind == DiscoveryCandidateKind::McpTool));
        assert!(candidates
            .iter()
            .any(|candidate| candidate.kind == DiscoveryCandidateKind::Skill));
        assert!(candidates.iter().all(|candidate| !candidate.handle_granted));
    }

    struct TimeoutSource;

    impl DiscoverySource for TimeoutSource {
        fn source_id(&self) -> &str {
            "slow_source"
        }

        fn timeout_ms(&self) -> u64 {
            0
        }

        fn scan(&self) -> Result<Vec<DiscoveryCandidate>, EvaError> {
            Ok(vec![DiscoveryCandidate::named(
                self.source_id(),
                DiscoveryCandidateKind::Workflow,
                "should-not-run",
                None,
                DiscoveryTrust::ConfiguredAllowlist,
            )])
        }
    }

    #[test]
    fn scan_sources_reports_timeout_without_candidates() {
        let source = TimeoutSource;
        let report = scan_sources(&[&source]);

        assert!(report.candidates.is_empty());
        assert_eq!(report.source_reports[0].status, "timeout");
        assert_eq!(
            report.source_reports[0].rejected_reason.as_deref(),
            Some("timeout_ms is zero")
        );
    }

    struct RejectedSource;

    impl DiscoverySource for RejectedSource {
        fn source_id(&self) -> &str {
            "rejected_source"
        }

        fn scan(&self) -> Result<Vec<DiscoveryCandidate>, EvaError> {
            Ok(vec![DiscoveryCandidate::named(
                self.source_id(),
                DiscoveryCandidateKind::Workflow,
                "disabled-workflow",
                None,
                DiscoveryTrust::DisplayOnly,
            )
            .rejected("workflow is disabled")])
        }
    }

    #[test]
    fn scan_sources_reports_candidate_rejections() {
        let source = RejectedSource;
        let report = scan_sources(&[&source]);

        assert_eq!(report.source_reports[0].status, "rejected");
        assert_eq!(
            report.source_reports[0].rejected_reason.as_deref(),
            Some("workflow is disabled")
        );
        assert!(report
            .candidates
            .iter()
            .all(|candidate| !candidate.handle_granted));
    }
}
