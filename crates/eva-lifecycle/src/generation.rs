//! Runtime generation state and handoff.

use eva_core::{EvaError, GenerationId};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "runtime generation state and handoff";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationState {
    Pending,
    Active,
    Draining,
    Retired,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeGeneration {
    pub id: GenerationId,
    pub release_ref: String,
    pub state: GenerationState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationController {
    pub active: RuntimeGeneration,
    pub candidate: Option<RuntimeGeneration>,
    pub retired: Vec<RuntimeGeneration>,
    pub audit: Vec<String>,
}

impl GenerationState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Retired => "retired",
            Self::Failed => "failed",
        }
    }
}

impl RuntimeGeneration {
    pub fn new(
        id: GenerationId,
        release_ref: impl Into<String>,
        state: GenerationState,
    ) -> Result<Self, EvaError> {
        let release_ref = release_ref.into();
        if release_ref.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "runtime release ref is required",
            ));
        }
        Ok(Self {
            id,
            release_ref,
            state,
        })
    }
}

impl GenerationController {
    pub fn new(active: RuntimeGeneration) -> Result<Self, EvaError> {
        if active.state != GenerationState::Active {
            return Err(
                EvaError::invalid_argument("initial generation must be active")
                    .with_context("generation", active.id.as_str())
                    .with_context("state", active.state.as_str()),
            );
        }
        Ok(Self {
            active,
            candidate: None,
            retired: Vec::new(),
            audit: vec!["generation:controller_created".to_owned()],
        })
    }

    pub fn start_candidate(&mut self, mut candidate: RuntimeGeneration) -> Result<(), EvaError> {
        if self.candidate.is_some() {
            return Err(EvaError::conflict("candidate generation already exists"));
        }
        candidate.state = GenerationState::Pending;
        self.audit
            .push(format!("candidate:{}:started", candidate.id.as_str()));
        self.candidate = Some(candidate);
        Ok(())
    }

    pub fn promote_candidate(&mut self) -> Result<(), EvaError> {
        let mut candidate = self
            .candidate
            .take()
            .ok_or_else(|| EvaError::not_found("candidate generation does not exist"))?;
        let mut old = self.active.clone();
        old.state = GenerationState::Draining;
        candidate.state = GenerationState::Active;
        self.audit
            .push(format!("generation:{}:promoted", candidate.id.as_str()));
        self.retired.push(old);
        self.active = candidate;
        Ok(())
    }

    pub fn fail_candidate(
        &mut self,
        reason: impl Into<String>,
    ) -> Result<RuntimeGeneration, EvaError> {
        let mut candidate = self
            .candidate
            .take()
            .ok_or_else(|| EvaError::not_found("candidate generation does not exist"))?;
        candidate.state = GenerationState::Failed;
        self.audit.push(format!(
            "candidate:{}:failed:{}",
            candidate.id.as_str(),
            reason.into()
        ));
        Ok(candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_promotes_candidate_and_drains_old_active() {
        let active = RuntimeGeneration::new(
            GenerationId::parse("gen-v13").unwrap(),
            "1.3.0",
            GenerationState::Active,
        )
        .unwrap();
        let mut controller = GenerationController::new(active).unwrap();
        let candidate = RuntimeGeneration::new(
            GenerationId::parse("gen-v14").unwrap(),
            "1.4.0",
            GenerationState::Pending,
        )
        .unwrap();

        controller.start_candidate(candidate).unwrap();
        controller.promote_candidate().unwrap();

        assert_eq!(controller.active.id.as_str(), "gen-v14");
        assert_eq!(controller.retired[0].state, GenerationState::Draining);
    }
}
