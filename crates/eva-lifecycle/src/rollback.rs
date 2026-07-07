//! Failed handoff rollback coordination.

use crate::GenerationDrainEvidence;
use eva_backup::RestorePlan;
use eva_core::{EvaError, GenerationId};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "rollback coordination after failed generation handoff";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackPlan {
    pub from_generation: GenerationId,
    pub to_generation: GenerationId,
    pub snapshot_id: Option<String>,
    pub reason: String,
    pub status: String,
    pub steps: Vec<String>,
    pub risks: Vec<String>,
    pub audit: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RollbackCoordinator;

impl RollbackCoordinator {
    pub fn plan_failed_handoff(
        &self,
        from_generation: GenerationId,
        to_generation: GenerationId,
        reason: impl Into<String>,
        restore_plan: Option<&RestorePlan>,
    ) -> Result<RollbackPlan, EvaError> {
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(EvaError::invalid_argument("rollback reason is required"));
        }
        let snapshot_id = restore_plan.map(|plan| plan.snapshot_id.clone());
        let mut risks =
            vec!["rollback is planned only; lifecycle apply remains explicit".to_owned()];
        if let Some(plan) = restore_plan {
            risks.extend(plan.risks.clone());
        } else {
            risks.push("no restore snapshot was attached".to_owned());
        }
        Ok(RollbackPlan {
            from_generation,
            to_generation,
            snapshot_id,
            reason,
            status: "planned".to_owned(),
            steps: vec![
                "stop candidate ingress".to_owned(),
                "keep previous generation active".to_owned(),
                "verify restore snapshot if present".to_owned(),
                "emit rollback audit".to_owned(),
            ],
            risks,
            audit: vec!["rollback:planned".to_owned()],
        })
    }

    pub fn plan_generation_lifecycle_rollback(
        &self,
        failed_generation: GenerationId,
        restored_generation: GenerationId,
        reason: impl Into<String>,
        drain_evidence: Option<&GenerationDrainEvidence>,
        restore_plan: Option<&RestorePlan>,
    ) -> Result<RollbackPlan, EvaError> {
        let mut plan = self.plan_failed_handoff(
            failed_generation.clone(),
            restored_generation.clone(),
            reason,
            restore_plan,
        )?;
        plan.steps.insert(
            0,
            "keep scheduler route on previous healthy generation".to_owned(),
        );
        plan.audit.push(format!(
            "rollback:generation:{}:to:{}",
            failed_generation.as_str(),
            restored_generation.as_str()
        ));
        if let Some(evidence) = drain_evidence {
            plan.audit.extend(evidence.audit.iter().map(|entry| {
                format!(
                    "rollback:observed_drain:{}:{}",
                    evidence.from_generation.as_str(),
                    entry
                )
            }));
        } else {
            plan.risks
                .push("no generation drain evidence was attached".to_owned());
        }
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollback_plan_preserves_previous_generation() {
        let plan = RollbackCoordinator
            .plan_failed_handoff(
                GenerationId::parse("gen-v14").unwrap(),
                GenerationId::parse("gen-v13").unwrap(),
                "candidate health failed",
                None,
            )
            .unwrap();

        assert_eq!(plan.status, "planned");
        assert!(plan.risks.iter().any(|risk| risk.contains("no restore")));
    }

    #[test]
    fn generation_lifecycle_rollback_carries_drain_audit() {
        let drain = crate::DrainCoordinator
            .plan_generation_swap_drain(
                GenerationId::parse("gen-old").unwrap(),
                GenerationId::parse("gen-new").unwrap(),
                1,
                30_000,
            )
            .unwrap();

        let plan = RollbackCoordinator
            .plan_generation_lifecycle_rollback(
                GenerationId::parse("gen-new").unwrap(),
                GenerationId::parse("gen-old").unwrap(),
                "candidate health failed",
                Some(&drain),
                None,
            )
            .unwrap();

        assert_eq!(
            plan.steps[0],
            "keep scheduler route on previous healthy generation"
        );
        assert!(plan
            .audit
            .iter()
            .any(|item| item == "rollback:generation:gen-new:to:gen-old"));
        assert!(plan.audit.iter().any(|item| {
            item == "rollback:observed_drain:gen-old:generation:gen-old:draining_after_swap_to:gen-new"
        }));
    }
}
