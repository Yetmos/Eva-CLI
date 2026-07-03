//! Runtime instance and summary state.

use crate::builder::{RuntimeMode, RuntimeOptions};
use crate::services::{RuntimeServices, ServiceSummary};
use crate::shutdown::{ShutdownReport, ShutdownState};
use eva_config::ProjectConfig;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "own the assembled Eva runtime instance";

/// V0.3 runtime lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStatus {
    Built,
    Shutdown,
}

/// Read-only summary shown by `eva inspect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSummary {
    pub mode: RuntimeMode,
    pub status: RuntimeStatus,
    pub generation_id: String,
    pub environment: String,
    pub project_root: String,
    pub agents_total: usize,
    pub agents_enabled: usize,
    pub adapters_total: usize,
    pub adapters_enabled: usize,
    pub capabilities_total: usize,
    pub capabilities_enabled: usize,
    pub routes_total: usize,
    pub policies_total: usize,
    pub services: Vec<ServiceSummary>,
}

/// Runtime owner for the current generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runtime {
    summary: RuntimeSummary,
    services: RuntimeServices,
    shutdown: ShutdownState,
}

impl RuntimeStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Built => "built",
            Self::Shutdown => "shutdown",
        }
    }
}

impl RuntimeSummary {
    pub fn from_project(
        project: &ProjectConfig,
        options: &RuntimeOptions,
        services: &RuntimeServices,
    ) -> Self {
        Self {
            mode: options.mode,
            status: RuntimeStatus::Built,
            generation_id: options.generation_id.as_str().to_owned(),
            environment: project.eva.runtime.env.clone(),
            project_root: project.project_root.display().to_string(),
            agents_total: project.agents.len(),
            agents_enabled: project.agents.iter().filter(|agent| agent.enabled).count(),
            adapters_total: project.adapters.len(),
            adapters_enabled: project
                .adapters
                .iter()
                .filter(|adapter| adapter.enabled)
                .count(),
            capabilities_total: project.capabilities.len(),
            capabilities_enabled: project
                .capabilities
                .iter()
                .filter(|capability| capability.enabled)
                .count(),
            routes_total: project.routes.routes.len(),
            policies_total: project.policies.len(),
            services: services.summaries().to_vec(),
        }
    }
}

impl Runtime {
    pub fn new(summary: RuntimeSummary, services: RuntimeServices) -> Self {
        Self {
            summary,
            services,
            shutdown: ShutdownState::default(),
        }
    }

    pub fn summary(&self) -> &RuntimeSummary {
        &self.summary
    }

    pub fn services(&self) -> &RuntimeServices {
        &self.services
    }

    /// Marks the no-op runtime as shutdown. The operation is idempotent.
    pub fn shutdown(&mut self) -> ShutdownReport {
        let report = self.shutdown.request();
        self.summary.status = RuntimeStatus::Shutdown;
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeBuilder;
    use eva_config::load_project_config;
    use std::path::{Path, PathBuf};

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    fn shutdown_is_idempotent() {
        let project = load_project_config(workspace_root()).unwrap();
        let mut runtime = RuntimeBuilder::new().build(&project).unwrap();

        let first = runtime.shutdown();
        let second = runtime.shutdown();

        assert!(!first.already_shutdown);
        assert!(second.already_shutdown);
        assert_eq!(runtime.summary().status, RuntimeStatus::Shutdown);
    }
}
