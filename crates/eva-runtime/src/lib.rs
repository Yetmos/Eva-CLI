//! 中文：Eva 运行时的组合根，统一导出构建、守护进程、恢复、诊断和任务边界。
//! Runtime composition root.

pub mod basic;
pub mod builder;
pub mod config_generation;
pub mod daemon;
pub mod diagnostics;
mod memory_worker;
pub mod recovery;
pub mod runtime;
pub mod scheduler_retry;
pub mod services;
pub mod shutdown;
pub mod task;
pub mod task_worker;

pub use basic::{BasicRunOptions, BasicRunReport};
pub use builder::{RuntimeBuilder, RuntimeMode, RuntimeOptions};
pub use config_generation::RuntimeConfigGeneration;
pub use daemon::{
    cleanup_failed_daemon_start, clear_daemon_startup_handshake, daemon_startup_report_path,
    daemon_status, read_daemon_startup_frame, read_daemon_startup_report,
    request_daemon_startup_abort, send_daemon_control_request, start_daemon,
    start_daemon_background_child, stop_daemon, write_daemon_startup_report,
    DaemonControlOperation, DaemonControlRequest, DaemonControlResponse, DaemonLeaseReport,
    DaemonMemoryMaintenanceReport, DaemonPathReport, DaemonPolicyReport, DaemonStartOptions,
    DaemonStartReport, DaemonStartupCleanupReport, DaemonStartupFrame, DaemonStartupHandshake,
    DaemonStartupPhase, DaemonStateRecord, DaemonStatusReport, DaemonStopReport,
    MAX_DAEMON_SHUTDOWN_DRAIN_TIMEOUT_MS,
};
pub use diagnostics::{
    inspect_durable_backend, DurableDiagnosticsOptions, DurableDiagnosticsReport,
};
pub use recovery::{
    ProviderBackoffTask, RecoveredEvent, RecoveredProviderProcess, RecoveredTask,
    RuntimeRecoveryCoordinator, RuntimeRecoveryOptions, RuntimeRecoveryReport, SkippedProviderTask,
    SkippedRedriveEvent, DEFAULT_PROVIDER_GRACEFUL_TERMINATION_TIMEOUT_MS,
};
pub use runtime::{Runtime, RuntimeStatus, RuntimeSummary};
pub use scheduler_retry::{
    run_scheduler_retry_tick, run_scheduler_retry_tick_with_handler, SchedulerRetryDispatchedEvent,
    SchedulerRetryFailedEvent, SchedulerRetrySkippedEvent, SchedulerRetryTickOptions,
    SchedulerRetryTickReport,
};
pub use services::{RuntimeServices, ServiceState, ServiceSummary};
pub use shutdown::{ShutdownReport, ShutdownState};
pub use task::{
    CancellationRecord, DeadLetterSummary, IdempotencyKey, ReplaySummary, RetryPolicy,
    TaskArtifactRef, TaskAttemptPolicy, TaskEnvelope, TaskInput, TaskKind, TaskLogEntry,
    TaskLogLevel, TaskReport, TaskStatus,
};
pub use task_worker::{
    FileSystemTaskArtifactResolver, OwnedReplayDeliveryStatus, OwnedReplayHandler,
    TaskArtifactResolver, TaskCancellationView, TaskHandler, TaskHandlerInvocation,
    TaskHandlerRegistry, TaskHandlerResult, TaskWorkerDrainOptions, TaskWorkerDrainReport,
    TaskWorkerRuntime, DEFAULT_TASK_ARTIFACT_INPUT_LIMIT_BYTES,
    DEFAULT_TASK_DRAIN_CANCELLATION_PERIOD_MS, DEFAULT_TASK_DRAIN_GRACE_PERIOD_MS,
    DEFAULT_TASK_HEARTBEAT_INTERVAL_MS, DEFAULT_TASK_WORKER_POLL_INTERVAL_MS,
    TASK_HANDLER_NOT_REGISTERED_MESSAGE,
};
