//! 运行时代际状态与交接。
//! Runtime generation state and handoff.

use eva_core::{EvaError, GenerationId};

/// 本模块的架构职责：维护运行时代际状态机及其交接记录。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "runtime generation state and handoff";

/// 一个运行时代际在交接生命周期中的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationState {
    /// 候选代际已创建但尚未通过健康门禁。
    Pending,
    /// 当前接收新任务的活动代际。
    Active,
    /// 已退出活动路由、正在等待在途任务结束的代际。
    Draining,
    /// 已安全退出生命周期的代际。
    Retired,
    /// 启动或健康验证失败的候选代际。
    Failed,
}

/// 运行时二进制发布在某一代际中的状态描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeGeneration {
    /// 稳定且唯一的代际标识。
    pub id: GenerationId,
    /// 该代际运行的发布引用。
    pub release_ref: String,
    /// 当前生命周期状态。
    pub state: GenerationState,
}

/// 管理单个活动代际、至多一个候选代际及历史代际的状态机。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationController {
    /// 当前承接新任务的代际。
    pub active: RuntimeGeneration,
    /// 正在启动或等待健康验证的候选代际。
    pub candidate: Option<RuntimeGeneration>,
    /// 已退出活动位置的历史代际；刚被替换时状态为排空中。
    pub retired: Vec<RuntimeGeneration>,
    /// 状态转换的有序审计记录。
    pub audit: Vec<String>,
}

impl GenerationState {
    /// 返回用于错误上下文和报告的稳定状态字符串。
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
    /// 创建具有非空发布引用的运行时代际描述。
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
    /// 以一个已经处于活动状态的代际初始化控制器。
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

    /// 注册新的候选代际，并强制将其置为等待验证状态。
    ///
    /// 同一时间只允许一个候选项，避免两次交接竞争活动位置。
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

    /// 将现有候选项提升为活动代际，并把旧活动代际转为排空中。
    ///
    /// 候选项通过外部健康门禁后才应调用。本方法一次更新内存状态，不会等待旧代际
    /// 排空；调用方必须继续收集排空证据后才能将旧代际视为安全退休。
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

    /// 移除候选项、标记失败并返回失败代际供调用方处置。
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
/// 运行时代际交接状态机测试。
mod tests {
    use super::*;

    #[test]
    /// 验证候选项提升后成为活动代际，旧代际进入排空状态。
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
