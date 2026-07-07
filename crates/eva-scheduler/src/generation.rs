//! Generation-aware route gating.

use eva_core::{EvaError, Event, GenerationId};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "gate scheduler routes by active or shadow-ready generation";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationRouteCandidate {
    pub generation_id: GenerationId,
    pub shadow_healthy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationRouteGate {
    active_generation: GenerationId,
    candidate: Option<GenerationRouteCandidate>,
    audit: Vec<String>,
}

impl GenerationRouteCandidate {
    pub fn shadowing(generation_id: GenerationId) -> Self {
        Self {
            generation_id,
            shadow_healthy: false,
        }
    }
}

impl GenerationRouteGate {
    pub fn new(active_generation: GenerationId) -> Self {
        Self {
            audit: vec![format!(
                "generation_route:{}:active",
                active_generation.as_str()
            )],
            active_generation,
            candidate: None,
        }
    }

    pub fn active_generation(&self) -> &GenerationId {
        &self.active_generation
    }

    pub fn candidate(&self) -> Option<&GenerationRouteCandidate> {
        self.candidate.as_ref()
    }

    pub fn audit(&self) -> &[String] {
        &self.audit
    }

    pub fn start_candidate(&mut self, generation_id: GenerationId) -> Result<(), EvaError> {
        if generation_id == self.active_generation {
            return Err(EvaError::conflict(
                "candidate generation must differ from active generation",
            )
            .with_context("generation", generation_id.as_str()));
        }
        if self.candidate.is_some() {
            return Err(EvaError::conflict(
                "candidate generation route already exists",
            ));
        }
        self.audit.push(format!(
            "generation_route:{}:shadowing",
            generation_id.as_str()
        ));
        self.candidate = Some(GenerationRouteCandidate::shadowing(generation_id));
        Ok(())
    }

    pub fn mark_candidate_shadow_healthy(
        &mut self,
        generation_id: &GenerationId,
    ) -> Result<(), EvaError> {
        let candidate = self.candidate.as_mut().ok_or_else(|| {
            EvaError::not_found("candidate generation route does not exist")
                .with_context("generation", generation_id.as_str())
        })?;
        if &candidate.generation_id != generation_id {
            return Err(
                EvaError::conflict("shadow health does not match candidate generation")
                    .with_context("candidate", candidate.generation_id.as_str())
                    .with_context("health", generation_id.as_str()),
            );
        }
        candidate.shadow_healthy = true;
        self.audit.push(format!(
            "generation_route:{}:shadow_healthy",
            generation_id.as_str()
        ));
        Ok(())
    }

    pub fn reject_candidate_shadow(
        &mut self,
        generation_id: &GenerationId,
        reason: impl Into<String>,
    ) -> Result<GenerationRouteCandidate, EvaError> {
        let candidate = self.candidate.take().ok_or_else(|| {
            EvaError::not_found("candidate generation route does not exist")
                .with_context("generation", generation_id.as_str())
        })?;
        if &candidate.generation_id != generation_id {
            self.candidate = Some(candidate.clone());
            return Err(
                EvaError::conflict("shadow rejection does not match candidate generation")
                    .with_context("candidate", candidate.generation_id.as_str())
                    .with_context("rejected", generation_id.as_str()),
            );
        }
        self.audit.push(format!(
            "generation_route:{}:shadow_rejected:{}",
            generation_id.as_str(),
            reason.into()
        ));
        Ok(candidate)
    }

    pub fn selected_generation_for_new_work(&self) -> &GenerationId {
        self.candidate
            .as_ref()
            .filter(|candidate| candidate.shadow_healthy)
            .map(|candidate| &candidate.generation_id)
            .unwrap_or(&self.active_generation)
    }

    pub fn stamp_new_work(&self, event: Event) -> Event {
        event.with_generation_id(self.selected_generation_for_new_work().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::{EventId, EventPayload, Topic};

    fn event(id: &str) -> Event {
        Event::new(
            EventId::parse(id).unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::empty(),
        )
    }

    #[test]
    fn route_gate_keeps_new_work_on_active_until_shadow_is_healthy() {
        let active = GenerationId::parse("gen-active").unwrap();
        let candidate = GenerationId::parse("gen-candidate").unwrap();
        let mut gate = GenerationRouteGate::new(active.clone());

        gate.start_candidate(candidate.clone()).unwrap();
        let before_health = gate.stamp_new_work(event("evt-before"));
        assert_eq!(before_health.metadata().generation_id(), Some(&active));

        gate.mark_candidate_shadow_healthy(&candidate).unwrap();
        let after_health = gate.stamp_new_work(event("evt-after"));
        assert_eq!(after_health.metadata().generation_id(), Some(&candidate));
        assert!(gate
            .audit()
            .iter()
            .any(|item| item == "generation_route:gen-candidate:shadow_healthy"));
    }

    #[test]
    fn route_gate_rejects_shadow_failure_and_keeps_active_generation() {
        let active = GenerationId::parse("gen-active").unwrap();
        let candidate = GenerationId::parse("gen-candidate").unwrap();
        let mut gate = GenerationRouteGate::new(active.clone());

        gate.start_candidate(candidate.clone()).unwrap();
        let rejected = gate
            .reject_candidate_shadow(&candidate, "shadow load failed")
            .unwrap();
        let routed = gate.stamp_new_work(event("evt-after-reject"));

        assert_eq!(rejected.generation_id, candidate);
        assert!(gate.candidate().is_none());
        assert_eq!(routed.metadata().generation_id(), Some(&active));
        assert!(gate.audit().iter().any(|item| {
            item == "generation_route:gen-candidate:shadow_rejected:shadow load failed"
        }));
    }
}
