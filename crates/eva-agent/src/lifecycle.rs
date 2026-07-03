//! Agent lifecycle state transitions.

use eva_core::EvaError;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Agent lifecycle state transitions";

/// Minimal lifecycle states for V0.4 AgentRuntime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentLifecycleState {
    Created,
    Running,
    Draining,
    Stopped,
    Failed,
}

impl AgentLifecycleState {
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

/// Mutable lifecycle guard for one Agent runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLifecycle {
    state: AgentLifecycleState,
}

impl Default for AgentLifecycle {
    fn default() -> Self {
        Self {
            state: AgentLifecycleState::Created,
        }
    }
}

impl AgentLifecycle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> AgentLifecycleState {
        self.state
    }

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

    pub fn stop(&mut self) {
        self.state = AgentLifecycleState::Stopped;
    }

    pub fn fail(&mut self) {
        self.state = AgentLifecycleState::Failed;
    }
}
