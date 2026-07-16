//! 中文：状态、事件日志、审计、任务、Provider 进程和制品的统一存储边界。
//! Storage boundary for state, event logs, and artifacts.

pub mod artifact_store;
pub mod audit_store;
pub mod durable_backend;
pub mod event_log;
pub mod provider_process;
pub mod sqlite;
pub mod state_store;
pub mod task_state;

pub use artifact_store::{
    ArtifactRecord, ArtifactStore, FileSystemArtifactStore, InMemoryArtifactStore,
};
pub use audit_store::{AuditRecord, FileSystemAuditSink};
pub use durable_backend::{
    migration_lock_is_held, DurableBackend, DurableBackendLayout, DurableBackendManifest,
    DurableBackendMode, DurableBackendOptions, DurableBackendReport, DurableWriterGuard,
    FileSystemDurableBackend, InMemoryDurableBackend, WriterGeneration,
    CURRENT_DURABLE_SCHEMA_VERSION, DURABLE_LAYOUT_VERSION,
};
pub use event_log::{
    EventLog, EventLogRecord, EventLogStatus, FileSystemEventLog, InMemoryEventLog,
};
pub use provider_process::{
    FileSystemProviderProcessTable, InMemoryProviderProcessTable, ProviderProcessSnapshot,
    ProviderProcessTable,
};
pub use state_store::{
    FileSystemStateStore, InMemoryStateStore, StateRecord, StateStore, StateVersion,
};
pub use task_state::{
    FileSystemTaskStateStore, TaskStateDeadLetterSnapshot, TaskStateLogSnapshot,
    TaskStateReplaySnapshot, TaskStateSnapshot, TaskStateStore,
};
