//! Adapter provider error mapping helpers.

use eva_core::EvaError;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "structured Adapter error mapping";

/// Maps a provider-private code and message into a sanitized Eva error.
pub fn provider_unavailable(
    adapter_id: &str,
    provider_code: &str,
    message: impl Into<String>,
) -> EvaError {
    EvaError::unavailable(message)
        .with_provider_code(provider_code)
        .with_context("adapter_id", adapter_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_mapping_preserves_safe_code() {
        let error = provider_unavailable("github-mcp", "EHOSTDOWN", "provider unavailable");

        assert_eq!(error.provider_code().unwrap().as_str(), "EHOSTDOWN");
        assert_eq!(error.context().entries()[0].1, "github-mcp");
    }
}
