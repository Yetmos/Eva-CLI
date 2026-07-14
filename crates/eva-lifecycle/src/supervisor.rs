//! Supervisor 进程与运行时所有权模型。
//! Supervisor process and runtime ownership.

use crate::generation::{GenerationController, GenerationState, RuntimeGeneration};
use eva_core::{EvaError, GenerationId};

/// 本模块的架构职责：维护 Supervisor 对运行时代际的唯一所有权。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "supervisor process and runtime ownership";

/// 某一候选运行时代际的健康探测结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeHealth {
    /// 探测结果所属代际，提交时必须与当前候选项一致。
    pub generation_id: GenerationId,
    /// 是否通过健康门禁。
    pub healthy: bool,
    /// 健康详情或失败原因。
    pub message: String,
}

/// Supervisor 当前代际所有权的只读报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorReport {
    /// 当前活动代际标识。
    pub active_generation: GenerationId,
    /// 等待提交的候选代际标识。
    pub candidate_generation: Option<GenerationId>,
    /// Supervisor 自身是否健康。
    pub healthy: bool,
    /// 代际状态转换的审计记录。
    pub audit: Vec<String>,
}

/// 基于内存状态机的 Supervisor 实现。
///
/// 该实现用于编排模型和测试，不启动或终止操作系统进程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InMemorySupervisor {
    /// Supervisor 独占的代际控制器。
    controller: GenerationController,
}

impl RuntimeHealth {
    /// 构造指定代际已通过健康检查的结果。
    pub fn healthy(generation_id: GenerationId) -> Self {
        Self {
            generation_id,
            healthy: true,
            message: "healthy".to_owned(),
        }
    }
}

impl InMemorySupervisor {
    /// 使用已处于活动状态的代际创建 Supervisor。
    pub fn new(active: RuntimeGeneration) -> Result<Self, EvaError> {
        Ok(Self {
            controller: GenerationController::new(active)?,
        })
    }

    /// 创建并注册等待健康验证的候选代际。
    pub fn start_candidate(
        &mut self,
        generation_id: GenerationId,
        release_ref: impl Into<String>,
    ) -> Result<(), EvaError> {
        let candidate =
            RuntimeGeneration::new(generation_id, release_ref, GenerationState::Pending)?;
        self.controller.start_candidate(candidate)
    }

    /// 在健康检查通过且代际标识匹配时提交候选项。
    ///
    /// 先验证健康布尔值，再验证结果确实属于当前候选代际，防止陈旧或串线的健康
    /// 响应提升错误代际。任一门禁失败都保持当前活动代际不变。
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

    /// 返回当前代际所有权与审计快照。
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
/// Supervisor 健康门禁与候选提交测试。
mod tests {
    use super::*;

    #[test]
    /// 验证匹配且健康的候选代际能够成为活动代际。
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
