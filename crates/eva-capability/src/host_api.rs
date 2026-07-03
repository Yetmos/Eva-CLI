//! Typed host API traits available to Lua and Agent runtimes.

use eva_core::{EvaError, InvokeRequest, InvokeResponse};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "typed host API traits available to Lua and Agent runtimes";

/// Minimal host API that controlled Lua and Agent runtimes can invoke.
pub trait CapabilityHostApi {
    fn invoke(&self, request: InvokeRequest) -> Result<InvokeResponse, EvaError>;
}
