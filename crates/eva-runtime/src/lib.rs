//! 中文：Eva 运行时的组合根，统一导出构建、守护进程、恢复、诊断和任务边界。
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
    DaemonControlRequest, DaemonControlResponse, DaemonMemoryMaintenanceReport, DaemonPathReport,
    DaemonPolicyReport, DaemonStartOptions, DaemonStartReport, DaemonStateRecord,
    DaemonStatusReport, DaemonStopReport,
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
    CancellationRecord, DeadLetterSummary, IdempotencyKey, ReplaySummary, RetryPolicy,
    TaskArtifactRef, TaskAttemptPolicy, TaskEnvelope, TaskInput, TaskKind, TaskLogEntry,
    TaskLogLevel, TaskReport, TaskStatus,
};
