//! Runtime composition root.

pub mod basic;
pub mod builder;
pub mod daemon;
pub mod diagnostics;
pub mod recovery;
pub mod runtime;
pub mod services;
pub mod shutdown;
pub mod task;

pub use basic::{BasicRunOptions, BasicRunReport};
pub use builder::{RuntimeBuilder, RuntimeMode, RuntimeOptions};
pub use daemon::{
    daemon_status, start_daemon, stop_daemon, DaemonPathReport, DaemonPolicyReport,
    DaemonStartOptions, DaemonStartReport, DaemonStateRecord, DaemonStatusReport, DaemonStopReport,
};
pub use diagnostics::{
    inspect_durable_backend, DurableDiagnosticsOptions, DurableDiagnosticsReport,
};
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
