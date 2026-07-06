//! Adapter registry, routing, and transport runtime boundary.

pub mod error;
pub mod manifest;
pub mod registry;
pub mod router;
pub mod runtime;
pub mod transports;

pub use manifest::{AdapterCapabilityBinding, AdapterHandle, AdapterHealth};
pub use registry::AdapterRegistry;
pub use router::{AdapterRoute, AdapterRouteRequest, AdapterRouter};
pub use runtime::{AdapterInvocation, AdapterInvokeReport, AdapterProbeReport, AdapterRuntime};
pub use transports::http::{
    HttpClient, HttpClientResponse, HttpInvocation, HttpMethod, HttpRunReport, HttpRunner,
    HttpRunnerConfig,
};
pub use transports::stdio::{
    StdioInvocation, StdioRunReport, StdioRunStatus, StdioRunner, StdioRunnerConfig,
};
