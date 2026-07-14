//! 按显式请求、清单提供者、默认提供者和回退提供者的优先级构造候选序列。
//!
//! 重复提供者会按首次出现去重，同时保留来源信息供权限校验和审计使用；选择只描述顺序，
//! 不在此处授予任何执行权限。
//! Capability provider selection planning.

use eva_core::{AdapterId, CapabilityName};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "capability provider selection metadata and deterministic plans";

/// 表示 `CapabilityProviderSelection` 数据结构。
/// Manifest-derived provider selection metadata for an adapter-backed capability.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapabilityProviderSelection {
    /// 记录 `manifest_provider` 字段对应的值。
    pub manifest_provider: Option<AdapterId>,
    /// 记录 `default_provider` 字段对应的值。
    pub default_provider: Option<AdapterId>,
    /// 记录 `fallback_providers` 字段对应的值。
    pub fallback_providers: Vec<AdapterId>,
    /// 记录 `required_adapter_capabilities` 字段对应的值。
    pub required_adapter_capabilities: Vec<CapabilityName>,
}

/// 定义 `CapabilityProviderSource` 可取的状态。
/// Why a provider appears in a selection plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityProviderSource {
    /// 表示 `ExplicitRequest` 枚举分支。
    ExplicitRequest,
    /// 表示 `ManifestProvider` 枚举分支。
    ManifestProvider,
    /// 表示 `DefaultProvider` 枚举分支。
    DefaultProvider,
    /// 表示 `FallbackProvider` 枚举分支。
    FallbackProvider,
}

/// 表示 `CapabilityProviderCandidate` 数据结构。
/// One provider candidate in deterministic invocation order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityProviderCandidate {
    /// 记录 `provider` 字段对应的值。
    pub provider: AdapterId,
    /// 记录 `source` 字段对应的值。
    pub source: CapabilityProviderSource,
}

/// 表示 `CapabilityProviderPlan` 数据结构。
/// Stable provider plan for one capability invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityProviderPlan {
    /// 记录 `capability` 字段对应的值。
    pub capability: CapabilityName,
    /// 记录 `providers` 字段对应的值。
    pub providers: Vec<CapabilityProviderCandidate>,
    /// 记录 `manifest_allowed_providers` 字段对应的值。
    pub manifest_allowed_providers: Vec<AdapterId>,
    /// 记录 `required_adapter_capabilities` 字段对应的值。
    pub required_adapter_capabilities: Vec<CapabilityName>,
}

impl CapabilityProviderSelection {
    /// 创建并初始化当前类型的实例。
    pub fn new(
        manifest_provider: Option<AdapterId>,
        default_provider: Option<AdapterId>,
        fallback_providers: Vec<AdapterId>,
        required_adapter_capabilities: Vec<CapabilityName>,
    ) -> Self {
        Self {
            manifest_provider,
            default_provider,
            fallback_providers,
            required_adapter_capabilities,
        }
    }

    /// 执行 `plan_for` 对应的处理逻辑。
    pub fn plan_for(
        &self,
        capability: CapabilityName,
        explicit_provider: Option<AdapterId>,
    ) -> CapabilityProviderPlan {
        let mut providers = Vec::new();
        let mut manifest_allowed_providers = Vec::new();
        if let Some(provider) = explicit_provider {
            push_unique(
                &mut providers,
                provider,
                CapabilityProviderSource::ExplicitRequest,
            );
        }
        if let Some(provider) = &self.manifest_provider {
            push_allowed(&mut manifest_allowed_providers, provider.clone());
            push_unique(
                &mut providers,
                provider.clone(),
                CapabilityProviderSource::ManifestProvider,
            );
        }
        if let Some(provider) = &self.default_provider {
            push_allowed(&mut manifest_allowed_providers, provider.clone());
            push_unique(
                &mut providers,
                provider.clone(),
                CapabilityProviderSource::DefaultProvider,
            );
        }
        for provider in &self.fallback_providers {
            push_allowed(&mut manifest_allowed_providers, provider.clone());
            push_unique(
                &mut providers,
                provider.clone(),
                CapabilityProviderSource::FallbackProvider,
            );
        }

        CapabilityProviderPlan {
            capability,
            providers,
            manifest_allowed_providers,
            required_adapter_capabilities: self.required_adapter_capabilities.clone(),
        }
    }
}

impl CapabilityProviderSource {
    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitRequest => "explicit_request",
            Self::ManifestProvider => "manifest_provider",
            Self::DefaultProvider => "default_provider",
            Self::FallbackProvider => "fallback_provider",
        }
    }
}

impl CapabilityProviderPlan {
    /// 执行 `provider_ids` 对应的处理逻辑。
    pub fn provider_ids(&self) -> impl Iterator<Item = &AdapterId> {
        self.providers.iter().map(|candidate| &candidate.provider)
    }

    /// 判断 `is_empty` 对应的条件是否成立。
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// 判断 `allows_manifest_provider` 对应的条件是否成立。
    pub fn allows_manifest_provider(&self, provider: &AdapterId) -> bool {
        self.manifest_allowed_providers.contains(provider)
    }
}

/// 登记 `push_allowed` 对应的数据或状态。
fn push_allowed(providers: &mut Vec<AdapterId>, provider: AdapterId) {
    if !providers.contains(&provider) {
        providers.push(provider);
    }
}

/// 登记 `push_unique` 对应的数据或状态。
fn push_unique(
    providers: &mut Vec<CapabilityProviderCandidate>,
    provider: AdapterId,
    source: CapabilityProviderSource,
) {
    if providers
        .iter()
        .any(|candidate| candidate.provider == provider)
    {
        return;
    }
    providers.push(CapabilityProviderCandidate { provider, source });
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;

    /// 执行 `adapter` 对应的处理逻辑。
    fn adapter(value: &str) -> AdapterId {
        AdapterId::parse(value).unwrap()
    }

    /// 执行 `capability` 对应的处理逻辑。
    fn capability(value: &str) -> CapabilityName {
        CapabilityName::parse(value).unwrap()
    }

    /// 验证 `provider_plan_uses_stable_precedence_and_deduplicates` 场景下的预期行为。
    #[test]
    fn provider_plan_uses_stable_precedence_and_deduplicates() {
        let selection = CapabilityProviderSelection::new(
            Some(adapter("manifest-provider")),
            Some(adapter("default-provider")),
            vec![
                adapter("fallback-a"),
                adapter("manifest-provider"),
                adapter("fallback-b"),
            ],
            vec![capability("repo.analyze")],
        );

        let plan = selection.plan_for(capability("repo.summary"), Some(adapter("fallback-a")));

        assert_eq!(
            plan.provider_ids()
                .map(AdapterId::as_str)
                .collect::<Vec<_>>(),
            [
                "fallback-a",
                "manifest-provider",
                "default-provider",
                "fallback-b"
            ]
        );
        assert_eq!(
            plan.providers
                .iter()
                .map(|candidate| candidate.source.as_str())
                .collect::<Vec<_>>(),
            [
                "explicit_request",
                "manifest_provider",
                "default_provider",
                "fallback_provider"
            ]
        );
        assert_eq!(
            plan.required_adapter_capabilities,
            [capability("repo.analyze")]
        );
        assert!(plan.allows_manifest_provider(&adapter("manifest-provider")));
        assert!(plan.allows_manifest_provider(&adapter("default-provider")));
        assert!(plan.allows_manifest_provider(&adapter("fallback-a")));
        assert!(!plan.allows_manifest_provider(&adapter("explicit-only")));
    }
}
