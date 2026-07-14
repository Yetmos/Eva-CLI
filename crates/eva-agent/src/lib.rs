//! 中文：Agent 生命周期、私有队列、执行控制和状态快照的运行时边界。
//! Agent runtime boundary.

pub mod lifecycle;
pub mod queue;
pub mod runtime;
pub mod state;

pub use lifecycle::{AgentLifecycle, AgentLifecycleState};
pub use queue::AgentQueue;
pub use runtime::{
    AgentHandlerOutput, AgentRunControl, AgentRunRecord, AgentRunStatus, AgentRuntime,
};
pub use state::AgentStateSnapshot;
