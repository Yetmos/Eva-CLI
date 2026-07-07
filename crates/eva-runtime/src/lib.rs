//! Runtime composition root.

pub mod basic;
pub mod builder;
pub mod recovery;
pub mod runtime;
pub mod services;
pub mod shutdown;
pub mod task;

pub use basic::{BasicRunOptions, BasicRunReport};
pub use builder::{RuntimeBuilder, RuntimeMode, RuntimeOptions};
pub use recovery::{
    RecoveredEvent, RecoveredTask, RuntimeRecoveryCoordinator, RuntimeRecoveryOptions,
    RuntimeRecoveryReport, SkippedRedriveEvent,
};
pub use runtime::{Runtime, RuntimeStatus, RuntimeSummary};
pub use services::{RuntimeServices, ServiceState, ServiceSummary};
pub use shutdown::{ShutdownReport, ShutdownState};
pub use task::{
    CancellationRecord, DeadLetterSummary, ReplaySummary, RetryPolicy, TaskLogEntry, TaskLogLevel,
    TaskReport, TaskStatus,
};
