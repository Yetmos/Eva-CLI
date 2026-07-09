//! Adapter registry, routing, and transport runtime boundary.

pub mod capability_host;
pub mod error;
pub mod manifest;
pub mod registry;
pub mod router;
pub mod runtime;
pub mod stream;
pub mod supervisor;
pub mod transports;

pub use capability_host::{
    is_retryable_provider_failure, response_from_report, AdapterBackedCapabilityHost,
};
pub use manifest::{
    AdapterCapabilityBinding, AdapterCircuitBreaker, AdapterHandle, AdapterHealth, AdapterRateLimit,
};
pub use registry::AdapterRegistry;
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
