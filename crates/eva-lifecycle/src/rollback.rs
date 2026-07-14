//! 失败交接后的回滚协调。
//! Failed handoff rollback coordination.

use crate::GenerationDrainEvidence;
use eva_backup::RestorePlan;
use eva_core::{EvaError, GenerationId};

/// 本模块的架构职责：为代际交接失败生成显式回滚计划。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "rollback coordination after failed generation handoff";

/// 失败交接回退到上一健康代际的计划与证据。
///
/// 该类型只描述后续动作，不表示任何运行时或文件恢复已经执行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackPlan {
    /// 发生故障、需要退出活动路径的代际。
    pub from_generation: GenerationId,
    /// 应恢复为活动状态的上一健康代际。
    pub to_generation: GenerationId,
    /// 可用于恢复持久化数据的备份快照标识。
    pub snapshot_id: Option<String>,
    /// 触发回滚的非空原因。
    pub reason: String,
    /// 当前回滚阶段；规划结果为 `planned`。
    pub status: String,
    /// 执行方应按顺序完成的回滚步骤。
    pub steps: Vec<String>,
    /// 缺失证据或恢复计划带来的风险。
    pub risks: Vec<String>,
    /// 回滚规划及关联排空证据的审计记录。
    pub audit: Vec<String>,
}

/// 生成回滚计划但不执行破坏性恢复的无状态协调器。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RollbackCoordinator;

impl RollbackCoordinator {
    /// 为失败的代际交接生成保留上一代际的基础回滚计划。
    ///
    /// 若提供恢复计划，则仅复制快照标识与风险；若缺失则明确记录风险。无论哪种
    /// 情况，本方法都不会应用快照或切换运行时，执行仍必须由显式生命周期流程完成。
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

    /// 扩展基础回滚计划，加入调度路由和旧代际排空证据。
    ///
    /// 首要步骤始终是让调度器保持在上一健康代际，避免故障候选继续接收新任务。
    /// 缺少排空证据不会隐式假定安全，而会作为风险暴露给执行方。
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
/// 回滚计划的上一代际保留与证据传播测试。
mod tests {
    use super::*;

    #[test]
    /// 验证基础回滚计划保留上一代际并标记缺少快照风险。
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
    /// 验证代际生命周期回滚携带旧代际排空审计证据。
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
