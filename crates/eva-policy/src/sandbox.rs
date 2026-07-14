//! 中文：Lua 与运行时沙箱的资源、库和副作用策略边界。
//! Lua and runtime sandbox policy boundaries.

use std::collections::BTreeSet;

/// 中文：多层合并时只能收紧的沙箱设置。
/// Sandbox settings that can only be narrowed when layers are merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPolicy {
    /// 中文：明确禁止加载的 Lua 标准库集合；合并时取并集。
    pub disabled_lua_libs: BTreeSet<String>,
    /// 中文：Lua 虚拟机可使用的内存上限（MiB）。
    pub memory_mb: Option<u32>,
    /// 中文：脚本执行的最大时长毫秒数。
    pub execution_timeout_ms: Option<u64>,
    /// 中文：是否允许脚本访问文件系统。
    pub filesystem_enabled: bool,
    /// 中文：是否允许脚本访问网络。
    pub network_enabled: bool,
    /// 中文：是否允许脚本读取宿主环境变量。
    pub environment_enabled: bool,
    /// 中文：是否必须校验脚本返回值结构。
    pub return_schema_validation: bool,
    /// 中文：是否必须校验脚本发出的事件主题。
    pub emitted_topic_validation: bool,
}

impl Default for SandboxPolicy {
    /// 中文：默认使用 Lua 安全基线，而非功能全开的运行环境。
    fn default() -> Self {
        Self::lua_default()
    }
}

impl SandboxPolicy {
    /// 中文：返回配置样例对应的安全 Lua 基线，关闭危险库和所有外部副作用。
    /// Returns the safe Lua baseline represented by `config/policies/sandbox.yaml`.
    pub fn lua_default() -> Self {
        Self {
            disabled_lua_libs: ["debug", "io", "os"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            memory_mb: Some(64),
            execution_timeout_ms: Some(30_000),
            filesystem_enabled: false,
            network_enabled: false,
            environment_enabled: false,
            return_schema_validation: true,
            emitted_topic_validation: true,
        }
    }

    /// 中文：把一个 Lua 库加入禁用集合。
    pub fn with_disabled_lua_lib(mut self, library: impl Into<String>) -> Self {
        self.disabled_lua_libs.insert(library.into());
        self
    }

    /// 中文：设置 Lua 虚拟机内存上限。
    pub fn with_memory_mb(mut self, value: u32) -> Self {
        self.memory_mb = Some(value);
        self
    }

    /// 中文：设置脚本执行时长上限。
    pub fn with_execution_timeout_ms(mut self, value: u64) -> Self {
        self.execution_timeout_ms = Some(value);
        self
    }

    /// 中文：设置文件系统访问开关。
    pub fn with_filesystem_enabled(mut self, value: bool) -> Self {
        self.filesystem_enabled = value;
        self
    }

    /// 中文：设置网络访问开关。
    pub fn with_network_enabled(mut self, value: bool) -> Self {
        self.network_enabled = value;
        self
    }

    /// 中文：设置环境变量读取开关。
    pub fn with_environment_enabled(mut self, value: bool) -> Self {
        self.environment_enabled = value;
        self
    }

    /// 中文：设置返回值结构校验要求。
    pub fn with_return_schema_validation(mut self, value: bool) -> Self {
        self.return_schema_validation = value;
        self
    }

    /// 中文：设置发出主题校验要求。
    pub fn with_emitted_topic_validation(mut self, value: bool) -> Self {
        self.emitted_topic_validation = value;
        self
    }

    /// 中文：返回两组沙箱策略中更严格的组合。
    ///
    /// 禁用库取并集，资源上限取较小值，副作用能力只有双方都允许才开放；安全校验只要
    /// 任一层要求就必须执行，因此任何后续层都不能关闭上游建立的校验门禁。
    /// Returns the stricter combination of two sandbox policies.
    pub fn narrowed_by(&self, other: &Self) -> Self {
        let mut disabled_lua_libs = self.disabled_lua_libs.clone();
        disabled_lua_libs.extend(other.disabled_lua_libs.iter().cloned());

        Self {
            disabled_lua_libs,
            memory_mb: min_option(self.memory_mb, other.memory_mb),
            execution_timeout_ms: min_option(self.execution_timeout_ms, other.execution_timeout_ms),
            filesystem_enabled: self.filesystem_enabled && other.filesystem_enabled,
            network_enabled: self.network_enabled && other.network_enabled,
            environment_enabled: self.environment_enabled && other.environment_enabled,
            return_schema_validation: self.return_schema_validation
                || other.return_schema_validation,
            emitted_topic_validation: self.emitted_topic_validation
                || other.emitted_topic_validation,
        }
    }

    /// 中文：判断指定 Lua 库是否未被禁用。
    pub fn permits_lua_lib(&self, library: &str) -> bool {
        !self.disabled_lua_libs.contains(library)
    }
}

/// 中文：合并可选资源上限；存在限制时保留限制，两侧都有限制时取更小值。
fn min_option<T: Ord + Copy>(left: Option<T>, right: Option<T>) -> Option<T> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// 中文：验证默认沙箱关闭危险库和外部副作用，并启用结构校验。
    fn default_sandbox_matches_safe_lua_floor() {
        let sandbox = SandboxPolicy::default();

        assert!(!sandbox.permits_lua_lib("os"));
        assert!(!sandbox.filesystem_enabled);
        assert!(!sandbox.network_enabled);
        assert!(sandbox.return_schema_validation);
        assert_eq!(sandbox.memory_mb, Some(64));
    }

    #[test]
    /// 中文：验证收窄会合并禁用库并采用更小资源上限。
    fn narrowing_unions_disabled_libs_and_uses_lower_limits() {
        let upper = SandboxPolicy::default()
            .with_disabled_lua_lib("package")
            .with_memory_mb(128)
            .with_execution_timeout_ms(60_000)
            .with_filesystem_enabled(true);
        let request = SandboxPolicy::default()
            .with_disabled_lua_lib("math")
            .with_memory_mb(32)
            .with_execution_timeout_ms(10_000)
            .with_filesystem_enabled(false);

        let effective = upper.narrowed_by(&request);

        assert!(!effective.permits_lua_lib("package"));
        assert!(!effective.permits_lua_lib("math"));
        assert_eq!(effective.memory_mb, Some(32));
        assert_eq!(effective.execution_timeout_ms, Some(10_000));
        assert!(!effective.filesystem_enabled);
    }

    #[test]
    /// 中文：验证任一策略层要求的校验都不会被后续层关闭。
    fn narrowing_keeps_required_validations_enabled() {
        let upper = SandboxPolicy::default().with_return_schema_validation(true);
        let request = SandboxPolicy::default().with_return_schema_validation(false);

        assert!(upper.narrowed_by(&request).return_schema_validation);
    }
}
