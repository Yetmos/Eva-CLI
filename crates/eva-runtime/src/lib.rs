//! Runtime composition root.

pub mod basic;
pub mod builder;
pub mod daemon;
pub mod diagnostics;
pub mod recovery;
pub mod runtime;
pub mod scheduler_retry;
pub mod services;
pub mod shutdown;
pub mod task;

pub use basic::{BasicRunOptions, BasicRunReport};
pub use builder::{RuntimeBuilder, RuntimeMode, RuntimeOptions};
pub use daemon::{
    daemon_status, send_daemon_control_request, start_daemon, stop_daemon, DaemonControlOperation,
    DaemonControlRequest, DaemonControlResponse, DaemonPathReport, DaemonPolicyReport,
    DaemonStartOptions, DaemonStartReport, DaemonStateRecord, DaemonStatusReport, DaemonStopReport,
};
pub use diagnostics::{
    inspect_durable_backend, DurableDiagnosticsOptions, DurableDiagnosticsReport,
};
pub use recovery::{
    ProviderBackoffTask, RecoveredEvent, RecoveredProviderProcess, RecoveredTask,
    RuntimeRecoveryCoordinator, RuntimeRecoveryOptions, RuntimeRecoveryReport, SkippedProviderTask,
    SkippedRedriveEvent,
};
pub use runtime::{Runtime, RuntimeStatus, RuntimeSummary};
pub use scheduler_retry::{
    run_scheduler_retry_tick, SchedulerRetryDispatchedEvent, SchedulerRetryFailedEvent,
    SchedulerRetrySkippedEvent, SchedulerRetryTickOptions, SchedulerRetryTickReport,
};
pub use services::{RuntimeServices, ServiceState, ServiceSummary};
pub use shutdown::{ShutdownReport, ShutdownState};
pub use task::{
    CancellationRecord, DeadLetterSummary, ReplaySummary, RetryPolicy, TaskLogEntry, TaskLogLevel,
    TaskReport, TaskStatus,
};
