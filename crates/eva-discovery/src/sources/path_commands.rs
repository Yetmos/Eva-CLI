//! PATH command discovery from trusted Adapter manifests.

use crate::normalizer::{DiscoveryCandidate, DiscoveryCandidateKind, DiscoveryTrust};
use crate::scanner::DiscoverySource;
use eva_config::{AdapterTransport, ProjectConfig};
use eva_core::EvaError;

pub const RESPONSIBILITY: &str = "discover trusted local commands from configured paths";

pub struct PathCommandDiscoverySource<'a> {
    project: &'a ProjectConfig,
}

impl<'a> PathCommandDiscoverySource<'a> {
    pub fn new(project: &'a ProjectConfig) -> Self {
        Self { project }
    }
}

impl DiscoverySource for PathCommandDiscoverySource<'_> {
    fn source_id(&self) -> &str {
        "path_commands"
    }

    fn timeout_ms(&self) -> u64 {
        250
    }

    fn scan(&self) -> Result<Vec<DiscoveryCandidate>, EvaError> {
        let mut candidates = Vec::new();
        for adapter in &self.project.adapters {
            if adapter.transport != AdapterTransport::Stdio {
                continue;
            }
            let Some(command) = adapter.extra_string("command") else {
                continue;
            };
            let mut candidate = DiscoveryCandidate::named(
                self.source_id(),
                DiscoveryCandidateKind::PathCommand,
                command,
                Some(adapter.id.clone()),
                DiscoveryTrust::ConfiguredAllowlist,
            );
            if !adapter.enabled {
                candidate = candidate.rejected("adapter manifest is disabled");
            } else if command.contains('/') || command.contains('\\') {
                candidate = candidate.rejected("PATH command source only records command names");
            }
            candidates.push(candidate);
        }
        Ok(candidates)
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
    fn path_command_source_reads_manifest_commands_without_handles() {
        let project = load_project_config(workspace_root()).unwrap();
        let source = PathCommandDiscoverySource::new(&project);
        let candidates = source.scan().unwrap();

        assert!(candidates.iter().any(|candidate| {
            candidate.kind == DiscoveryCandidateKind::PathCommand
                && candidate.id == "path_command:codex-cli:codex"
        }));
        assert!(candidates.iter().all(|candidate| !candidate.handle_granted));
    }
}
