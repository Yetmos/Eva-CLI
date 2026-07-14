//! 本模块提供 `error` 相关实现。
//! Adapter provider error mapping helpers.

use eva_core::EvaError;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "structured Adapter error mapping";

/// 执行 `provider_unavailable` 对应的处理逻辑。
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

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 `provider_mapping_preserves_safe_code` 场景下的预期行为。
    #[test]
    fn provider_mapping_preserves_safe_code() {
        let error = provider_unavailable("github-mcp", "EHOSTDOWN", "provider unavailable");

        assert_eq!(error.provider_code().unwrap().as_str(), "EHOSTDOWN");
        assert_eq!(error.context().entries()[0].1, "github-mcp");
    }
}
