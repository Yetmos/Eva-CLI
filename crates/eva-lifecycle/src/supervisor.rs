//! Supervisor process and runtime ownership.

use crate::generation::{GenerationController, GenerationState, RuntimeGeneration};
use eva_core::{EvaError, GenerationId};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "supervisor process and runtime ownership";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeHealth {
    pub generation_id: GenerationId,
    pub healthy: bool,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorReport {
    pub active_generation: GenerationId,
    pub candidate_generation: Option<GenerationId>,
    pub healthy: bool,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InMemorySupervisor {
    controller: GenerationController,
}

impl RuntimeHealth {
    pub fn healthy(generation_id: GenerationId) -> Self {
        Self {
            generation_id,
            healthy: true,
            message: "healthy".to_owned(),
        }
    }
}

impl InMemorySupervisor {
    pub fn new(active: RuntimeGeneration) -> Result<Self, EvaError> {
        Ok(Self {
            controller: GenerationController::new(active)?,
        })
    }

    pub fn start_candidate(
        &mut self,
        generation_id: GenerationId,
        release_ref: impl Into<String>,
    ) -> Result<(), EvaError> {
        let candidate =
            RuntimeGeneration::new(generation_id, release_ref, GenerationState::Pending)?;
        self.controller.start_candidate(candidate)
    }

    pub fn commit_candidate(&mut self, health: RuntimeHealth) -> Result<(), EvaError> {
        if !health.healthy {
            return Err(EvaError::unavailable("candidate runtime is not healthy")
                .with_context("generation", health.generation_id.as_str()));
        }
        let candidate = self
            .controller
            .candidate
            .as_ref()
            .ok_or_else(|| EvaError::not_found("candidate generation does not exist"))?;
        if candidate.id != health.generation_id {
            return Err(
                EvaError::conflict("health check does not match candidate generation")
                    .with_context("candidate", candidate.id.as_str())
                    .with_context("health", health.generation_id.as_str()),
            );
        }
        self.controller.promote_candidate()
    }

    pub fn report(&self) -> SupervisorReport {
        SupervisorReport {
            active_generation: self.controller.active.id.clone(),
            candidate_generation: self
                .controller
                .candidate
                .as_ref()
                .map(|candidate| candidate.id.clone()),
            healthy: true,
            audit: self.controller.audit.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supervisor_commits_healthy_candidate() {
        let active = RuntimeGeneration::new(
            GenerationId::parse("gen-v13").unwrap(),
            "1.3.0",
            GenerationState::Active,
        )
        .unwrap();
        let mut supervisor = InMemorySupervisor::new(active).unwrap();

        supervisor
            .start_candidate(GenerationId::parse("gen-v14").unwrap(), "1.4.0")
            .unwrap();
        supervisor
            .commit_candidate(RuntimeHealth::healthy(
                GenerationId::parse("gen-v14").unwrap(),
            ))
            .unwrap();

        assert_eq!(supervisor.report().active_generation.as_str(), "gen-v14");
    }
}
