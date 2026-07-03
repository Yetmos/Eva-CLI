//! Runtime builder for V0.3 no-op composition.

use crate::runtime::{Runtime, RuntimeSummary};
use crate::services::RuntimeServices;
use eva_config::ProjectConfig;
use eva_core::{EvaError, GenerationId};
use std::fmt;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "compose concrete runtime services from validated configuration";

/// Runtime execution mode selected by the composition root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMode {
    /// V0.3 mode: build summaries and boundaries without starting side effects.
    Noop,
    /// V0.4 mode: wire the in-memory event loop for example execution.
    InMemoryV04,
}

/// Runtime builder options that are stable enough for CLI inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeOptions {
    pub mode: RuntimeMode,
    pub generation_id: GenerationId,
}

/// Builds an Eva runtime from already validated project configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeBuilder {
    options: RuntimeOptions,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self::noop()
    }
}

impl Default for RuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Noop => "noop",
            Self::InMemoryV04 => "in_memory_v0.4",
        }
    }
}

impl fmt::Display for RuntimeMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl RuntimeOptions {
    /// Returns the V0.3 no-op runtime options.
    pub fn noop() -> Self {
        Self {
            mode: RuntimeMode::Noop,
            generation_id: GenerationId::parse("noop-v0.3")
                .expect("static V0.3 generation id is valid"),
        }
    }

    /// Returns V0.4 in-memory runtime options.
    pub fn in_memory_v04() -> Self {
        Self {
            mode: RuntimeMode::InMemoryV04,
            generation_id: GenerationId::parse("basic-v0.4")
                .expect("static V0.4 generation id is valid"),
        }
    }

    pub fn with_generation_id(mut self, generation_id: GenerationId) -> Self {
        self.generation_id = generation_id;
        self
    }
}

impl RuntimeBuilder {
    pub fn new() -> Self {
        Self {
            options: RuntimeOptions::default(),
        }
    }

    pub fn in_memory_v04() -> Self {
        Self {
            options: RuntimeOptions::in_memory_v04(),
        }
    }

    pub fn with_options(options: RuntimeOptions) -> Self {
        Self { options }
    }

    /// Builds a no-op runtime summary from validated configuration.
    pub fn build(&self, project: &ProjectConfig) -> Result<Runtime, EvaError> {
        if project.agents.is_empty() {
            return Err(
                EvaError::invalid_argument("runtime requires at least one Agent manifest")
                    .with_context("field", "agents"),
            );
        }
        if project.routes.routes.is_empty() {
            return Err(
                EvaError::invalid_argument("runtime requires at least one route")
                    .with_context("field", "routes"),
            );
        }

        let services = match self.options.mode {
            RuntimeMode::Noop => RuntimeServices::noop(project),
            RuntimeMode::InMemoryV04 => RuntimeServices::in_memory_v04(project),
        };
        let summary = RuntimeSummary::from_project(project, &self.options, &services);
        Ok(Runtime::new(summary, services))
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
    fn noop_builder_summarizes_sample_project() {
        let project = load_project_config(workspace_root()).unwrap();
        let runtime = RuntimeBuilder::new().build(&project).unwrap();
        let summary = runtime.summary();

        assert_eq!(summary.mode, RuntimeMode::Noop);
        assert_eq!(summary.generation_id, "noop-v0.3");
        assert_eq!(summary.agents_total, project.agents.len());
        assert!(summary
            .services
            .iter()
            .any(|service| service.name == "config"));
    }
}
