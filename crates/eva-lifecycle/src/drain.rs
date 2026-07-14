//! 旧运行时代际的排空规划。
//! Draining old runtime generations.

use eva_core::{EvaError, GenerationId};

/// 本模块的架构职责：阻止旧代际接收新任务并记录排空证据。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "draining old runtime generations";

/// 旧代际排空计划的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainStatus {
    /// 已停止接收新任务，但仍有进行中的任务。
    Planned,
    /// 已没有进行中的任务，可以安全退休。
    Completed,
    /// 在预算内未完成排空，需要上层决定强制终止或回滚。
    TimedOut,
}

/// 单个运行时代际的排空计划。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainPlan {
    /// 正在排空的旧代际标识。
    pub generation_id: GenerationId,
    /// 规划时仍在执行的任务数量。
    pub inflight_tasks: usize,
    /// 等待任务完成的最长时间，单位为毫秒。
    pub timeout_ms: u64,
    /// 是否仍允许接收新任务；排空计划中固定为 `false`。
    pub accepts_new_work: bool,
    /// 当前排空状态。
    pub status: DrainStatus,
    /// 排空状态转换的有序审计记录。
    pub audit: Vec<String>,
}

/// 代际切换后旧代际排空的关联证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationDrainEvidence {
    /// 被移出活动路由并开始排空的旧代际。
    pub from_generation: GenerationId,
    /// 已接管新任务的目标代际。
    pub to_generation: GenerationId,
    /// 旧代际的具体排空计划。
    pub plan: DrainPlan,
    /// 包含代际关联信息的审计记录。
    pub audit: Vec<String>,
}

/// 构造和推进排空计划的无状态协调器。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DrainCoordinator;

impl DrainStatus {
    /// 返回用于报告和审计的稳定状态字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Completed => "completed",
            Self::TimedOut => "timed_out",
        }
    }
}

impl DrainCoordinator {
    /// 创建立即停止接收新任务的排空计划。
    ///
    /// 零任务计划可直接标为完成；仍有任务时只进入计划状态，本方法不会等待、取消
    /// 或终止任务。零超时无法提供安全排空窗口，因此作为无效参数拒绝。
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

    /// 为一次明确的代际切换创建旧代际排空证据。
    ///
    /// 源与目标必须不同。审计记录把排空动作和新代际绑定，供回滚流程判断旧代际
    /// 是否曾被停止接收任务；本方法仍不执行真实调度器切换。
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

    /// 将排空计划标记为已完成，并把在途任务数归零。
    pub fn complete(&self, mut plan: DrainPlan) -> DrainPlan {
        plan.inflight_tasks = 0;
        plan.status = DrainStatus::Completed;
        plan.audit.push("drain:completed".to_owned());
        plan
    }

    /// 将排空计划标记为超时，保留在途任务数供上层评估风险。
    pub fn timeout(&self, mut plan: DrainPlan) -> DrainPlan {
        plan.status = DrainStatus::TimedOut;
        plan.audit.push("drain:timed_out".to_owned());
        plan
    }
}

#[cfg(test)]
/// 排空计划状态与代际关联证据测试。
mod tests {
    use super::*;

    #[test]
    /// 验证规划后立即停止新任务，并可显式完成排空。
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
    /// 验证代际切换证据明确阻止旧代际接收新任务。
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
    /// 验证没有在途任务时切换排空可立即完成。
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
