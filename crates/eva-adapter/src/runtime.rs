//! Authorized Adapter runtime probes and controlled invocation envelopes.

use crate::registry::AdapterRegistry;
use crate::router::{AdapterRouteRequest, AdapterRouter};
use crate::transports;
use eva_config::{AdapterTransport, ProjectConfig};
use eva_core::{AdapterId, CapabilityName, EvaError, RequestId};
use eva_observability::{SpanId, TraceFields};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "authorized transport execution with timeout and audit";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterInvocation {
    pub request_id: RequestId,
    pub capability: CapabilityName,
    pub provider: Option<AdapterId>,
    pub input: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterProbeReport {
    pub adapter_id: AdapterId,
    pub transport: AdapterTransport,
    pub status: String,
    pub capabilities: Vec<CapabilityName>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterInvokeReport {
    pub request_id: RequestId,
    pub adapter_id: AdapterId,
    pub transport: AdapterTransport,
    pub capability: CapabilityName,
    pub status: String,
    pub output: String,
    pub audit: Vec<String>,
    pub trace: TraceFields,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterRuntime {
    registry: AdapterRegistry,
    router: AdapterRouter,
}

impl AdapterInvocation {
    pub fn new(request_id: RequestId, capability: CapabilityName) -> Self {
        Self {
            request_id,
            capability,
            provider: None,
            input: String::new(),
        }
    }

    pub fn with_provider(mut self, provider: AdapterId) -> Self {
        self.provider = Some(provider);
        self
    }

    pub fn with_input(mut self, input: impl Into<String>) -> Self {
        self.input = input.into();
        self
    }

    pub fn trace_for_adapter(&self, adapter_id: &AdapterId) -> TraceFields {
        TraceFields::default()
            .with_request_id(self.request_id.clone())
            .with_adapter_id(adapter_id.clone())
            .with_capability(self.capability.clone())
            .with_provider(adapter_id.as_str())
            .with_span_id(
                SpanId::parse("adapter.invoke")
                    .expect("static adapter span identifiers use the observability character set"),
            )
    }
}

impl AdapterRuntime {
    pub fn from_project(project: &ProjectConfig) -> Result<Self, EvaError> {
        let registry = AdapterRegistry::from_project(project)?;
        let router = AdapterRouter::new(registry.clone());
        Ok(Self { registry, router })
    }

    pub fn registry(&self) -> &AdapterRegistry {
        &self.registry
    }

    pub fn router(&self) -> &AdapterRouter {
        &self.router
    }

    pub fn list(&self) -> Vec<&crate::manifest::AdapterHandle> {
        self.registry.list()
    }

    pub fn probe_adapter(&self, adapter_id: &AdapterId) -> Result<AdapterProbeReport, EvaError> {
        let handle = self.registry.get(adapter_id).ok_or_else(|| {
            EvaError::not_found("Adapter provider does not exist")
                .with_context("adapter_id", adapter_id.as_str())
        })?;
        Ok(AdapterProbeReport {
            adapter_id: handle.id.clone(),
            transport: handle.transport,
            status: handle.health().as_str().to_owned(),
            capabilities: handle.capabilities.clone(),
            detail: if handle.enabled {
                "authorized handle is registered; probe has no external side effects".to_owned()
            } else {
                "adapter manifest is disabled".to_owned()
            },
        })
    }

    pub fn probe_capability(
        &self,
        capability: CapabilityName,
        provider: Option<AdapterId>,
    ) -> Result<AdapterProbeReport, EvaError> {
        let mut request = AdapterRouteRequest::new(capability);
        if let Some(provider) = provider {
            request = request.with_provider(provider);
        }
        let route = self.router.route(&request)?;
        self.probe_adapter(&route.handle.id)
    }

    pub fn invoke(&self, invocation: AdapterInvocation) -> Result<AdapterInvokeReport, EvaError> {
        let mut request = AdapterRouteRequest::new(invocation.capability.clone());
        if let Some(provider) = invocation.provider.clone() {
            request = request.with_provider(provider);
        }
        let route = self.router.route(&request)?;
        let handle = route.handle;

        match handle.transport {
            AdapterTransport::Mcp => transports::mcp::invoke(&handle, invocation),
            AdapterTransport::Skill => transports::skill::invoke(&handle, invocation),
            AdapterTransport::Builtin
            | AdapterTransport::LuaCapability
            | AdapterTransport::Eventbus => transports::builtin::invoke(&handle, invocation),
            AdapterTransport::Hardware => transports::hardware::invoke(&handle, invocation),
            AdapterTransport::Stdio | AdapterTransport::Http => Err(EvaError::unsupported(
                "Adapter transport requires an external executor not started in this version",
            )
            .with_context("adapter_id", handle.id.as_str())
            .with_context("transport", handle.transport.as_str())
            .with_context(
                "suggestion",
                "use adapter probe/list for diagnostics or a skill/MCP controlled envelope",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use std::path::{Path, PathBuf};

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    fn runtime_invokes_skill_adapter_as_controlled_envelope() {
        let project = load_project_config(workspace_root()).unwrap();
        let runtime = AdapterRuntime::from_project(&project).unwrap();
        let invocation = AdapterInvocation::new(
            RequestId::parse("req-skill-1").unwrap(),
            CapabilityName::parse("workflow.code_review").unwrap(),
        )
        .with_provider(AdapterId::parse("code-review-skill").unwrap())
        .with_input("current_diff");

        let report = runtime.invoke(invocation).unwrap();

        assert_eq!(report.status, "completed");
        assert!(report.output.contains("code-review"));
        assert_eq!(
            report.trace.request_id.as_ref().map(|id| id.as_str()),
            Some("req-skill-1")
        );
        assert_eq!(
            report.trace.adapter_id.as_ref().map(|id| id.as_str()),
            Some("code-review-skill")
        );
        assert_eq!(
            report
                .trace
                .capability
                .as_ref()
                .map(|capability| capability.as_str()),
            Some("workflow.code_review")
        );
    }
}
