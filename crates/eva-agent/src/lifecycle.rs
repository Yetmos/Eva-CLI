//! 中文：Agent 生命周期状态及其受约束的转换规则。
//! Agent lifecycle state transitions.

use eva_core::EvaError;

/// 中文：本模块集中管理 Agent 生命周期转换，避免调用方任意改写状态。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Agent lifecycle state transitions";

/// 中文：AgentRuntime 支持的最小生命周期状态集合。
/// Minimal lifecycle states for V0.4 AgentRuntime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentLifecycleState {
    /// 中文：实例已创建，尚未开始接收事件。
    Created,
    /// 中文：实例正在运行，可以接收和处理事件。
    Running,
    /// 中文：实例正在排空存量工作，不应接收新任务。
    Draining,
    /// 中文：实例已正常停止，可由显式启动操作再次运行。
    Stopped,
    /// 中文：实例因不可恢复错误失败，需要外部恢复或重建。
    Failed,
}

impl AgentLifecycleState {
    /// 中文：返回用于日志、诊断和稳定协议输出的状态名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::Draining => "draining",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }
}

/// 中文：单个 Agent 运行时的可变生命周期守卫，只暴露合法的状态转换操作。
/// Mutable lifecycle guard for one Agent runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLifecycle {
    /// 中文：当前生命周期状态，外部只能读取，不能直接覆盖。
    state: AgentLifecycleState,
}

impl Default for AgentLifecycle {
    /// 中文：新生命周期默认处于 `Created`，不会隐式开始执行。
    fn default() -> Self {
        Self {
            state: AgentLifecycleState::Created,
        }
    }
}

impl AgentLifecycle {
    /// 中文：创建处于初始状态的生命周期守卫。
    pub fn new() -> Self {
        Self::default()
    }

    /// 中文：返回当前状态的副本。
    pub fn state(&self) -> AgentLifecycleState {
        self.state
    }

    /// 中文：从 `Created` 或 `Stopped` 进入运行态；其他状态启动会产生冲突错误。
    pub fn start(&mut self) -> Result<(), EvaError> {
        match self.state {
            AgentLifecycleState::Created | AgentLifecycleState::Stopped => {
                self.state = AgentLifecycleState::Running;
                Ok(())
            }
            _ => Err(
                EvaError::conflict("agent cannot start from current lifecycle state")
                    .with_context("state", self.state.as_str()),
            ),
        }
    }

    /// 中文：只允许运行中的 Agent 进入排空态，确保排空语义不会被重复或越级触发。
    pub fn drain(&mut self) -> Result<(), EvaError> {
        match self.state {
            AgentLifecycleState::Running => {
                self.state = AgentLifecycleState::Draining;
                Ok(())
            }
            _ => Err(
                EvaError::conflict("agent cannot drain from current lifecycle state")
                    .with_context("state", self.state.as_str()),
            ),
        }
    }

    /// 中文：无条件标记为正常停止；由调用方负责先完成必要的排空和资源释放。
    pub fn stop(&mut self) {
        self.state = AgentLifecycleState::Stopped;
    }

    /// 中文：无条件标记为失败，供执行边界记录不可恢复故障。
    pub fn fail(&mut self) {
        self.state = AgentLifecycleState::Failed;
    }
}
