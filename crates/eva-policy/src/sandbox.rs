//! Lua and runtime sandbox policy boundaries.

use std::collections::BTreeSet;

/// Sandbox settings that can only be narrowed when layers are merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPolicy {
    pub disabled_lua_libs: BTreeSet<String>,
    pub memory_mb: Option<u32>,
    pub execution_timeout_ms: Option<u64>,
    pub filesystem_enabled: bool,
    pub network_enabled: bool,
    pub environment_enabled: bool,
    pub return_schema_validation: bool,
    pub emitted_topic_validation: bool,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self::lua_default()
    }
}

impl SandboxPolicy {
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

    pub fn with_disabled_lua_lib(mut self, library: impl Into<String>) -> Self {
        self.disabled_lua_libs.insert(library.into());
        self
    }

    pub fn with_memory_mb(mut self, value: u32) -> Self {
        self.memory_mb = Some(value);
        self
    }

    pub fn with_execution_timeout_ms(mut self, value: u64) -> Self {
        self.execution_timeout_ms = Some(value);
        self
    }

    pub fn with_filesystem_enabled(mut self, value: bool) -> Self {
        self.filesystem_enabled = value;
        self
    }

    pub fn with_network_enabled(mut self, value: bool) -> Self {
        self.network_enabled = value;
        self
    }

    pub fn with_environment_enabled(mut self, value: bool) -> Self {
        self.environment_enabled = value;
        self
    }

    pub fn with_return_schema_validation(mut self, value: bool) -> Self {
        self.return_schema_validation = value;
        self
    }

    pub fn with_emitted_topic_validation(mut self, value: bool) -> Self {
        self.emitted_topic_validation = value;
        self
    }

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

    pub fn permits_lua_lib(&self, library: &str) -> bool {
        !self.disabled_lua_libs.contains(library)
    }
}

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
    fn default_sandbox_matches_safe_lua_floor() {
        let sandbox = SandboxPolicy::default();

        assert!(!sandbox.permits_lua_lib("os"));
        assert!(!sandbox.filesystem_enabled);
        assert!(!sandbox.network_enabled);
        assert!(sandbox.return_schema_validation);
        assert_eq!(sandbox.memory_mb, Some(64));
    }

    #[test]
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
    fn narrowing_keeps_required_validations_enabled() {
        let upper = SandboxPolicy::default().with_return_schema_validation(true);
        let request = SandboxPolicy::default().with_return_schema_validation(false);

        assert!(upper.narrowed_by(&request).return_schema_validation);
    }
}
