//! Capability provider selection planning.

use eva_core::{AdapterId, CapabilityName};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "capability provider selection metadata and deterministic plans";

/// Manifest-derived provider selection metadata for an adapter-backed capability.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapabilityProviderSelection {
    pub manifest_provider: Option<AdapterId>,
    pub default_provider: Option<AdapterId>,
    pub fallback_providers: Vec<AdapterId>,
    pub required_adapter_capabilities: Vec<CapabilityName>,
}

/// Why a provider appears in a selection plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityProviderSource {
    ExplicitRequest,
    ManifestProvider,
    DefaultProvider,
    FallbackProvider,
}

/// One provider candidate in deterministic invocation order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityProviderCandidate {
    pub provider: AdapterId,
    pub source: CapabilityProviderSource,
}

/// Stable provider plan for one capability invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityProviderPlan {
    pub capability: CapabilityName,
    pub providers: Vec<CapabilityProviderCandidate>,
    pub manifest_allowed_providers: Vec<AdapterId>,
    pub required_adapter_capabilities: Vec<CapabilityName>,
}

impl CapabilityProviderSelection {
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
    pub fn provider_ids(&self) -> impl Iterator<Item = &AdapterId> {
        self.providers.iter().map(|candidate| &candidate.provider)
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    pub fn allows_manifest_provider(&self, provider: &AdapterId) -> bool {
        self.manifest_allowed_providers.contains(provider)
    }
}

fn push_allowed(providers: &mut Vec<AdapterId>, provider: AdapterId) {
    if !providers.contains(&provider) {
        providers.push(provider);
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(value: &str) -> AdapterId {
        AdapterId::parse(value).unwrap()
    }

    fn capability(value: &str) -> CapabilityName {
        CapabilityName::parse(value).unwrap()
    }

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
