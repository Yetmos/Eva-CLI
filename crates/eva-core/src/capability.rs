//! 中文：Capability 名称与 provider 选择提示的纯数据契约。
//! English: Pure data contracts for capability names and provider selection hints.

use crate::error::EvaError;
use crate::ids::AdapterId;
use std::fmt;
use std::str::FromStr;

/// 中文：点分段 capability 名称，例如 `repo.summary`。
/// English: Dot-separated capability name, such as `repo.summary`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapabilityName {
    // 中文：保留原始稳定名称，Display/日志/配置回写都使用这一份字符串。
    // English: Keep the stable original name for Display, logs, and config round-tripping.
    name: String,
    // 中文：预拆分分段，避免下游重复解析 namespace。
    // English: Store parsed segments so downstream code does not repeatedly split namespaces.
    segments: Vec<String>,
}

impl CapabilityName {
    /// 中文：解析并校验点分段 capability 名称。
    /// English: Parses and validates a dot-separated capability name.
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        validate_capability_name(value)?;
        Ok(Self {
            name: value.to_owned(),
            segments: value.split('.').map(str::to_owned).collect(),
        })
    }

    /// 中文：从 owned 或 borrowed 字符串创建 capability 名称。
    /// English: Creates a capability name from an owned or borrowed string.
    pub fn new(value: impl Into<String>) -> Result<Self, EvaError> {
        let value = value.into();
        Self::parse(&value)
    }

    /// 中文：返回已校验的稳定 capability 名称。
    /// English: Returns the validated stable capability name.
    pub fn as_str(&self) -> &str {
        &self.name
    }

    /// 中文：返回第一个分段，供 scheduler/registry 做 namespace 粗分流。
    /// English: Returns the first segment for coarse scheduler/registry namespace routing.
    pub fn namespace(&self) -> &str {
        &self.segments[0]
    }

    /// 中文：返回所有点分段，顺序与原始名称一致。
    /// English: Returns all dot-separated segments in original order.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.segments.iter().map(String::as_str)
    }

    /// 中文：判断 capability 是否属于指定 namespace；这里不做模糊匹配。
    /// English: Returns true when the capability belongs to the namespace; no fuzzy matching is applied.
    pub fn starts_with_namespace(&self, namespace: &str) -> bool {
        self.namespace() == namespace
    }
}

impl fmt::Display for CapabilityName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

impl FromStr for CapabilityName {
    type Err = EvaError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for CapabilityName {
    type Error = EvaError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

/// 中文：选择 provider 的非强制提示；真正路由仍由上层 registry/router 决定。
/// English: Non-binding provider selection hint; actual routing belongs to upper registry/router layers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProviderHint {
    /// 中文：优先使用指定 Adapter。
    /// English: Prefer a specific Adapter.
    Adapter(AdapterId),
    /// 中文：优先使用命名 provider；这里仅保存数据，不解释 provider 协议。
    /// English: Prefer a named provider; this value is data only and does not interpret provider protocols.
    Named(String),
}

impl ProviderHint {
    /// 中文：在名称非空且稳定时创建命名 provider hint。
    /// English: Creates a named provider hint when the supplied value is non-empty and stable.
    pub fn named(value: impl Into<String>) -> Result<Self, EvaError> {
        let value = value.into();
        validate_provider_name(&value)?;
        Ok(Self::Named(value))
    }
}

impl fmt::Display for ProviderHint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Adapter(id) => write!(f, "adapter:{id}"),
            Self::Named(name) => f.write_str(name),
        }
    }
}

/// 中文：Capability 名称加可选 provider 偏好，表示“想调用什么”和“倾向谁执行”。
/// English: Capability name plus an optional provider preference, separating what to invoke from who may run it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CapabilityRef {
    name: CapabilityName,
    provider_hint: Option<ProviderHint>,
}

impl CapabilityRef {
    /// 中文：创建无 provider hint 的 capability 引用。
    /// English: Creates a capability reference without a provider hint.
    pub fn new(name: CapabilityName) -> Self {
        Self {
            name,
            provider_hint: None,
        }
    }

    /// 中文：创建带 provider hint 的 capability 引用。
    /// English: Creates a capability reference with a provider hint.
    pub fn with_provider_hint(name: CapabilityName, provider_hint: ProviderHint) -> Self {
        Self {
            name,
            provider_hint: Some(provider_hint),
        }
    }

    /// 中文：返回 capability 名称。
    /// English: Returns the capability name.
    pub fn name(&self) -> &CapabilityName {
        &self.name
    }

    /// 中文：返回 provider hint；没有 hint 时由上层自由选择 provider。
    /// English: Returns the provider hint; when absent, upper layers are free to choose a provider.
    pub fn provider_hint(&self) -> Option<&ProviderHint> {
        self.provider_hint.as_ref()
    }
}

fn validate_capability_name(value: &str) -> Result<(), EvaError> {
    // 中文：Capability 名称会出现在配置和路由键里，因此拒绝空值与首尾空白。
    // English: Capability names appear in config and routing keys, so reject empty values and edge whitespace.
    if value.is_empty() {
        return Err(capability_error(value, "capability name cannot be empty"));
    }

    if value.trim() != value {
        return Err(capability_error(
            value,
            "capability name cannot contain leading or trailing whitespace",
        ));
    }

    // 中文：逐段校验，避免 `repo..summary` 这类不稳定 namespace。
    // English: Validate segment by segment to avoid unstable namespaces such as `repo..summary`.
    for segment in value.split('.') {
        validate_capability_segment(value, segment)?;
    }

    Ok(())
}

fn validate_capability_segment(full_name: &str, segment: &str) -> Result<(), EvaError> {
    if segment.is_empty() {
        return Err(capability_error(
            full_name,
            "capability name cannot contain empty segments",
        ));
    }

    // 中文：限制为 ASCII 稳定字符，保证跨平台配置、日志和 CLI 参数一致。
    // English: Restrict to stable ASCII characters for consistent config, logs, and CLI arguments across platforms.
    if !segment
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err(capability_error(
            full_name,
            "capability segments may only contain ASCII letters, digits, '_', and '-'",
        ));
    }

    Ok(())
}

fn validate_provider_name(value: &str) -> Result<(), EvaError> {
    // 中文：Provider hint 是可选路由提示，不应携带空白分隔的命令或参数片段。
    // English: Provider hints are optional routing data and must not carry whitespace-separated commands or arguments.
    if value.is_empty() || value.trim() != value || value.chars().any(char::is_whitespace) {
        return Err(
            EvaError::invalid_argument("provider hint must be a non-empty stable name")
                .with_context("provider_hint", value),
        );
    }

    Ok(())
}

fn capability_error(value: &str, message: &str) -> EvaError {
    EvaError::invalid_argument(message).with_context("capability", value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_name_accepts_dot_segments() {
        let name = CapabilityName::parse("repo.summary").unwrap();
        assert_eq!(name.as_str(), "repo.summary");
        assert_eq!(name.to_string(), "repo.summary");
        assert_eq!(name.segments().collect::<Vec<_>>(), ["repo", "summary"]);
    }

    #[test]
    fn capability_name_rejects_empty_segment() {
        let error = CapabilityName::parse("repo..summary").unwrap_err();
        assert!(error.message().contains("empty segments"));
    }

    #[test]
    fn capability_name_exposes_namespace() {
        let name = CapabilityName::parse("repo.summary").unwrap();
        assert_eq!(name.namespace(), "repo");
        assert!(name.starts_with_namespace("repo"));
        assert!(!name.starts_with_namespace("workflow"));
    }

    #[test]
    fn capability_name_rejects_unstable_chars() {
        assert!(CapabilityName::parse("repo.summary/read").is_err());
        assert!(CapabilityName::parse("repo.摘要").is_err());
    }

    #[test]
    fn capability_ref_keeps_provider_hint_optional() {
        let name = CapabilityName::parse("workflow.code_review").unwrap();
        let without_hint = CapabilityRef::new(name.clone());
        assert_eq!(without_hint.name(), &name);
        assert!(without_hint.provider_hint().is_none());

        let hint = ProviderHint::Adapter(AdapterId::parse("adapter-review").unwrap());
        let with_hint = CapabilityRef::with_provider_hint(name, hint);
        assert!(matches!(
            with_hint.provider_hint(),
            Some(ProviderHint::Adapter(_))
        ));
    }

    #[test]
    fn named_provider_hint_is_data_only() {
        let hint = ProviderHint::named("codex-cli").unwrap();
        assert_eq!(hint.to_string(), "codex-cli");
    }
}
