//! Capability routing before provider execution.

use crate::host_api::CapabilityHostApi;
use crate::registry::{CapabilityDescriptor, CapabilityRegistry};
use crate::selection::CapabilityProviderPlan;
use eva_core::{AdapterId, EvaError, InvokeOutput, InvokeRequest, InvokeResponse, InvokeTarget};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "capability routing before provider execution";

/// V0.4 router for builtin capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityRouter {
    registry: CapabilityRegistry,
}

impl CapabilityRouter {
    pub fn new(registry: CapabilityRegistry) -> Self {
        Self { registry }
    }

    pub fn with_v04_builtins() -> Self {
        Self::new(CapabilityRegistry::with_v04_builtins())
    }

    pub fn registry(&self) -> &CapabilityRegistry {
        &self.registry
    }

    pub fn provider_plan(
        &self,
        request: &InvokeRequest,
        explicit_provider: Option<AdapterId>,
    ) -> Result<CapabilityProviderPlan, EvaError> {
        let descriptor = self.descriptor_for_request(request)?;
        self.ensure_enabled(descriptor)?;
        Ok(descriptor.provider_plan(explicit_provider))
    }

    fn invoke_descriptor(
        &self,
        descriptor: &CapabilityDescriptor,
        request: InvokeRequest,
    ) -> Result<InvokeResponse, EvaError> {
        self.ensure_enabled(descriptor)?;

        let text = request.input().as_text().unwrap_or_default();
        let output = match descriptor.name.as_str() {
            "config.lint" => format!(
                "{{\"valid\":true,\"findings\":[],\"input\":\"{}\"}}",
                escape_json(text)
            ),
            "runtime.echo" => format!("{{\"echo\":\"{}\"}}", escape_json(text)),
            value => {
                return Err(EvaError::unsupported("capability has no builtin provider")
                    .with_context("capability", value))
            }
        };
        Ok(InvokeResponse::completed(
            request.request_id().clone(),
            InvokeOutput::text(output),
        ))
    }

    fn descriptor_for_request(
        &self,
        request: &InvokeRequest,
    ) -> Result<&CapabilityDescriptor, EvaError> {
        let capability = match request.target() {
            InvokeTarget::Capability(capability) => capability,
            _ => {
                return Err(EvaError::invalid_argument(
                    "capability router requires capability target",
                ))
            }
        };
        self.registry.get(capability).ok_or_else(|| {
            EvaError::not_found("capability is not registered")
                .with_context("capability", capability.as_str())
        })
    }

    fn ensure_enabled(&self, descriptor: &CapabilityDescriptor) -> Result<(), EvaError> {
        if descriptor.enabled {
            return Ok(());
        }

        Err(EvaError::permission_denied("capability is disabled")
            .with_context("capability", descriptor.name.as_str()))
    }
}

impl CapabilityHostApi for CapabilityRouter {
    fn invoke(&self, request: InvokeRequest) -> Result<InvokeResponse, EvaError> {
        let descriptor = self.descriptor_for_request(&request)?;
        self.invoke_descriptor(descriptor, request)
    }
}

fn escape_json(value: &str) -> String {
    let mut escaped = String::new();
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            value => escaped.push(value),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::CapabilityRegistry;
    use eva_core::{CapabilityId, CapabilityName, ErrorKind, InvokeInput, RequestId};

    #[test]
    fn builtin_config_lint_returns_completed_response() {
        let router = CapabilityRouter::with_v04_builtins();
        let request = InvokeRequest::new(
            RequestId::parse("req-1").unwrap(),
            InvokeTarget::Capability(CapabilityName::parse("config.lint").unwrap()),
            InvokeInput::text("config"),
        );

        let response = router.invoke(request).unwrap();

        assert!(response.is_success());
        assert!(response
            .output()
            .unwrap()
            .as_text()
            .unwrap()
            .contains("valid"));
    }

    #[test]
    fn provider_plan_rejects_disabled_capability_before_selection() {
        let mut registry = CapabilityRegistry::new();
        registry
            .register(CapabilityDescriptor {
                id: CapabilityId::parse("runtime-echo-disabled").unwrap(),
                name: CapabilityName::parse("runtime.echo").unwrap(),
                enabled: false,
                provider: "builtin".to_owned(),
                provider_selection: Default::default(),
            })
            .unwrap();
        let router = CapabilityRouter::new(registry);
        let request = InvokeRequest::new(
            RequestId::parse("req-disabled-plan").unwrap(),
            InvokeTarget::Capability(CapabilityName::parse("runtime.echo").unwrap()),
            InvokeInput::text("hello"),
        );

        let error = router.provider_plan(&request, None).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }
}
