//! 本模块提供 `lib` 相关实现。
//! Adapter registry, routing, and transport runtime boundary.

/// 声明 `capability_host` 子模块。
pub mod capability_host;
/// 声明 `error` 子模块。
pub mod error;
/// 声明 `manifest` 子模块。
pub mod manifest;
/// Central OS-owned provider process spawning and supervision boundary.
pub mod process_backend;
/// 声明 `registry` 子模块。
pub mod registry;
/// Durable provider restart policy and backoff calculation.
pub mod restart;
/// 声明 `router` 子模块。
pub mod router;
/// 声明 `runtime` 子模块。
pub mod runtime;
/// 声明 `stream` 子模块。
pub mod stream;
/// 声明 `supervisor` 子模块。
pub mod supervisor;
/// 声明 `transports` 子模块。
pub mod transports;

pub use capability_host::{
    is_retryable_provider_failure, response_from_report, AdapterBackedCapabilityHost,
};
pub use manifest::{
    AdapterCapabilityBinding, AdapterCircuitBreaker, AdapterHandle, AdapterHealth, AdapterRateLimit,
};
pub use process_backend::{
    OsProcessBackend, ProcessBackend, ProcessIdentity, ProcessTerminationOutcome,
    ProcessTerminationReport, ProviderProcessHandle, ProviderProcessSpawner,
};
pub use registry::AdapterRegistry;
pub use restart::{
    decide_restart, due_at_ms as restart_due_at_ms, RestartDecision, RestartOutcome,
    DEFAULT_STABLE_RUN_WINDOW_MS, MAX_RESTART_BACKOFF_MS,
};
pub use router::{AdapterRoute, AdapterRouteRequest, AdapterRouter};
pub use runtime::{AdapterInvocation, AdapterInvokeReport, AdapterProbeReport, AdapterRuntime};
pub use stream::{
    capture_provider_bytes, collect_provider_stream, default_provider_artifact_root,
    provider_stream_audit, provider_stream_key, provider_stream_summary_json,
    ProviderStreamArtifact, ProviderStreamCapture, ProviderStreamConfig,
};
pub use supervisor::{
    InMemoryProviderSupervisor, ProviderCredentialScope, ProviderExecutionOutcome,
    ProviderExecutionRequest, ProviderExecutionSlot, ProviderSupervisor, PROVIDER_SESSION_ID_ENV,
    PROVIDER_SESSION_ID_HEADER, PROVIDER_SESSION_TOKEN_ENV, PROVIDER_SESSION_TOKEN_HEADER,
};
pub use transports::http::{
    HttpClient, HttpClientResponse, HttpInvocation, HttpMethod, HttpRunReport, HttpRunner,
    HttpRunnerConfig,
};
pub use transports::skill::{
    SkillArtifactEvidence, SkillRunReport, SkillRunStatus, SkillRunner, SkillRunnerConfig,
    SkillRunnerInvocation,
};
pub use transports::stdio::{
    StdioInvocation, StdioRunReport, StdioRunStatus, StdioRunner, StdioRunnerConfig,
};
