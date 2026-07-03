//! Capability routing before provider execution.

use crate::host_api::CapabilityHostApi;
use crate::registry::{CapabilityDescriptor, CapabilityRegistry};
use eva_core::{EvaError, InvokeOutput, InvokeRequest, InvokeResponse, InvokeTarget};

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

    fn invoke_descriptor(
        &self,
        descriptor: &CapabilityDescriptor,
        request: InvokeRequest,
    ) -> Result<InvokeResponse, EvaError> {
        if !descriptor.enabled {
            return Err(EvaError::permission_denied("capability is disabled")
                .with_context("capability", descriptor.name.as_str()));
        }

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
}

impl CapabilityHostApi for CapabilityRouter {
    fn invoke(&self, request: InvokeRequest) -> Result<InvokeResponse, EvaError> {
        let capability = match request.target() {
            InvokeTarget::Capability(capability) => capability,
            _ => {
                return Err(EvaError::invalid_argument(
                    "capability router requires capability target",
                ))
            }
        };
        let descriptor = self.registry.get(capability).ok_or_else(|| {
            EvaError::not_found("capability is not registered")
                .with_context("capability", capability.as_str())
        })?;
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
    use eva_core::{CapabilityName, InvokeInput, RequestId};

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
}
