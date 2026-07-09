//! Typed host API traits available to Lua and Agent runtimes.

use eva_core::{AdapterId, EvaError, InvokeRequest, InvokeResponse};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "typed host API traits available to Lua and Agent runtimes";

/// Minimal host API that controlled Lua and Agent runtimes can invoke.
pub trait CapabilityHostApi {
    fn invoke(&self, request: InvokeRequest) -> Result<InvokeResponse, EvaError>;

    fn invoke_with_provider(
        &self,
        request: InvokeRequest,
        explicit_provider: Option<AdapterId>,
    ) -> Result<InvokeResponse, EvaError> {
        if let Some(provider) = explicit_provider {
            return Err(EvaError::unsupported(
                "capability host does not support explicit provider routing",
            )
            .with_context("provider", provider.as_str()));
        }
        self.invoke(request)
    }
}
