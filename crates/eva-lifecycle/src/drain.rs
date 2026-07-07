//! Draining old runtime generations.

use eva_core::{EvaError, GenerationId};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "draining old runtime generations";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainStatus {
    Planned,
    Completed,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainPlan {
    pub generation_id: GenerationId,
    pub inflight_tasks: usize,
    pub timeout_ms: u64,
    pub accepts_new_work: bool,
    pub status: DrainStatus,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationDrainEvidence {
    pub from_generation: GenerationId,
    pub to_generation: GenerationId,
    pub plan: DrainPlan,
    pub audit: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DrainCoordinator;

impl DrainStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Completed => "completed",
            Self::TimedOut => "timed_out",
        }
    }
}

impl DrainCoordinator {
    pub fn plan(
        &self,
        generation_id: GenerationId,
        inflight_tasks: usize,
        timeout_ms: u64,
    ) -> Result<DrainPlan, EvaError> {
        if timeout_ms == 0 {
            return Err(EvaError::invalid_argument("drain timeout must be positive"));
        }
        Ok(DrainPlan {
            generation_id,
            inflight_tasks,
            timeout_ms,
            accepts_new_work: false,
            status: if inflight_tasks == 0 {
                DrainStatus::Completed
            } else {
                DrainStatus::Planned
            },
            audit: vec!["drain:planned".to_owned()],
        })
    }

    pub fn plan_generation_swap_drain(
        &self,
        from_generation: GenerationId,
        to_generation: GenerationId,
        inflight_tasks: usize,
        timeout_ms: u64,
    ) -> Result<GenerationDrainEvidence, EvaError> {
        if from_generation == to_generation {
            return Err(EvaError::conflict(
                "generation drain requires distinct source and target generations",
            )
            .with_context("generation", from_generation.as_str()));
        }
        let mut plan = self.plan(from_generation.clone(), inflight_tasks, timeout_ms)?;
        plan.audit.push(format!(
            "generation:{}:draining_after_swap_to:{}",
            from_generation.as_str(),
            to_generation.as_str()
        ));
        Ok(GenerationDrainEvidence {
            from_generation,
            to_generation,
            audit: plan.audit.clone(),
            plan,
        })
    }

    pub fn complete(&self, mut plan: DrainPlan) -> DrainPlan {
        plan.inflight_tasks = 0;
        plan.status = DrainStatus::Completed;
        plan.audit.push("drain:completed".to_owned());
        plan
    }

    pub fn timeout(&self, mut plan: DrainPlan) -> DrainPlan {
        plan.status = DrainStatus::TimedOut;
        plan.audit.push("drain:timed_out".to_owned());
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_plan_stops_new_work_and_can_complete() {
        let plan = DrainCoordinator
            .plan(GenerationId::parse("gen-v13").unwrap(), 2, 30_000)
            .unwrap();

        assert!(!plan.accepts_new_work);
        assert_eq!(plan.status, DrainStatus::Planned);

        let completed = DrainCoordinator.complete(plan);
        assert_eq!(completed.status, DrainStatus::Completed);
        assert_eq!(completed.inflight_tasks, 0);
    }

    #[test]
    fn generation_swap_drain_evidence_blocks_new_work_on_old_generation() {
        let evidence = DrainCoordinator
            .plan_generation_swap_drain(
                GenerationId::parse("gen-old").unwrap(),
                GenerationId::parse("gen-new").unwrap(),
                2,
                30_000,
            )
            .unwrap();

        assert_eq!(evidence.from_generation.as_str(), "gen-old");
        assert_eq!(evidence.to_generation.as_str(), "gen-new");
        assert!(!evidence.plan.accepts_new_work);
        assert_eq!(evidence.plan.status, DrainStatus::Planned);
        assert!(evidence
            .audit
            .iter()
            .any(|item| { item == "generation:gen-old:draining_after_swap_to:gen-new" }));
    }

    #[test]
    fn generation_swap_drain_completes_when_no_inflight_tasks_remain() {
        let evidence = DrainCoordinator
            .plan_generation_swap_drain(
                GenerationId::parse("gen-old").unwrap(),
                GenerationId::parse("gen-new").unwrap(),
                0,
                30_000,
            )
            .unwrap();

        assert_eq!(evidence.plan.status, DrainStatus::Completed);
        assert_eq!(evidence.plan.inflight_tasks, 0);
    }
}
