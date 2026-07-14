//! 中文：基于代际的路由闸门，确保候选代际通过影子健康检查后才接收新任务。
//! Generation-aware route gating.

use eva_core::{EvaError, Event, GenerationId};

/// 中文：本模块负责在活动代际与已就绪候选代际之间安全选择新任务去向。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "gate scheduler routes by active or shadow-ready generation";

/// 中文：正在进行影子验证、尚未正式接管流量的候选代际。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationRouteCandidate {
    /// 中文：候选运行时的稳定代际标识。
    pub generation_id: GenerationId,
    /// 中文：候选代际是否已完成影子加载和健康检查。
    pub shadow_healthy: bool,
}

/// 中文：代际路由状态机；同时最多保留一个候选代际，并记录可审计的切换轨迹。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationRouteGate {
    /// 中文：当前承接生产任务的代际。
    active_generation: GenerationId,
    /// 中文：可选的影子候选；未健康时不得接收新任务。
    candidate: Option<GenerationRouteCandidate>,
    /// 中文：按发生顺序保存的代际切换审计记录。
    audit: Vec<String>,
}

impl GenerationRouteCandidate {
    /// 中文：创建初始为“不健康”的影子候选，健康状态必须由显式检查推进。
    pub fn shadowing(generation_id: GenerationId) -> Self {
        Self {
            generation_id,
            shadow_healthy: false,
        }
    }
}

impl GenerationRouteGate {
    /// 中文：以给定代际建立路由闸门，并写入首条活动代际审计记录。
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

    /// 中文：返回当前活动代际；该值在候选影子验证期间保持不变。
    pub fn active_generation(&self) -> &GenerationId {
        &self.active_generation
    }

    /// 中文：返回正在验证的候选代际（若有）。
    pub fn candidate(&self) -> Option<&GenerationRouteCandidate> {
        self.candidate.as_ref()
    }

    /// 中文：返回只读审计轨迹，供诊断和发布门禁使用。
    pub fn audit(&self) -> &[String] {
        &self.audit
    }

    /// 中文：启动一个新的影子候选。
    ///
    /// 候选必须不同于活动代际，且同一时刻只能有一个候选，避免健康结果被错误地
    /// 关联到另一轮发布。状态校验通过后才写审计记录和候选状态。
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

    /// 中文：将指定候选标记为影子健康，使后续新任务可以路由到该代际。
    ///
    /// 健康结果携带的代际必须与当前候选完全一致，防止迟到的探针结果推进错误版本。
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

    /// 中文：拒绝指定影子候选并恢复“仅活动代际”路由。
    ///
    /// 若传入标识不匹配，会把临时取出的候选放回，保证一次错误的拒绝请求不会丢失
    /// 正在验证的状态；成功拒绝时返回候选快照供上层执行清理。
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

    /// 中文：选择新任务应绑定的代际；只有健康候选才能优先于活动代际。
    pub fn selected_generation_for_new_work(&self) -> &GenerationId {
        self.candidate
            .as_ref()
            .filter(|candidate| candidate.shadow_healthy)
            .map(|candidate| &candidate.generation_id)
            .unwrap_or(&self.active_generation)
    }

    /// 中文：把当前路由选择写入事件元数据，使下游处理链使用同一代际。
    pub fn stamp_new_work(&self, event: Event) -> Event {
        event.with_generation_id(self.selected_generation_for_new_work().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::{EventId, EventPayload, Topic};

    /// 中文：构造带稳定输入主题的测试事件。
    fn event(id: &str) -> Event {
        Event::new(
            EventId::parse(id).unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::empty(),
        )
    }

    #[test]
    /// 中文：验证候选健康前后，新任务分别绑定活动代际和候选代际。
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
    /// 中文：验证影子失败会移除候选，且新任务继续留在活动代际。
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
