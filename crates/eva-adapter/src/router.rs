//! Adapter provider selection by explicit provider or capability index.

use crate::manifest::AdapterHandle;
use crate::registry::AdapterRegistry;
use eva_core::{AdapterId, CapabilityName, EvaError};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "provider selection by explicit provider or capability";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterRouteRequest {
    pub capability: CapabilityName,
    pub provider: Option<AdapterId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterRoute {
    pub handle: AdapterHandle,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterRouter {
    registry: AdapterRegistry,
}

impl AdapterRouteRequest {
    pub fn new(capability: CapabilityName) -> Self {
        Self {
            capability,
            provider: None,
        }
    }

    pub fn with_provider(mut self, provider: AdapterId) -> Self {
        self.provider = Some(provider);
        self
    }
}

impl AdapterRouter {
    pub fn new(registry: AdapterRegistry) -> Self {
        Self { registry }
    }

    pub fn registry(&self) -> &AdapterRegistry {
        &self.registry
    }

    pub fn route(&self, request: &AdapterRouteRequest) -> Result<AdapterRoute, EvaError> {
        if let Some(provider) = &request.provider {
            let handle = self.registry.get(provider).ok_or_else(|| {
                EvaError::not_found("Adapter provider does not exist")
                    .with_context("provider", provider.as_str())
            })?;
            return self.route_handle(handle, &request.capability, "explicit provider");
        }

        let mut providers = self.registry.providers_for_capability(&request.capability);
        providers.sort_by(|left, right| left.id.cmp(&right.id));
        let handle = providers
            .into_iter()
            .find(|handle| handle.enabled)
            .ok_or_else(|| {
                EvaError::not_found("no enabled Adapter provider supports capability")
                    .with_context("capability", request.capability.as_str())
            })?;
        self.route_handle(handle, &request.capability, "capability index")
    }

    fn route_handle(
        &self,
        handle: &AdapterHandle,
        capability: &CapabilityName,
        reason: &'static str,
    ) -> Result<AdapterRoute, EvaError> {
        if !handle.enabled {
            return Err(EvaError::permission_denied("Adapter provider is disabled")
                .with_context("adapter_id", handle.id.as_str()));
        }
        if !handle.supports(capability) {
            return Err(
                EvaError::permission_denied("Adapter provider does not expose capability")
                    .with_context("adapter_id", handle.id.as_str())
                    .with_context("capability", capability.as_str()),
            );
        }
        Ok(AdapterRoute {
            handle: handle.clone(),
            reason: reason.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use std::path::{Path, PathBuf};

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    fn router_uses_explicit_provider_when_present() {
        let project = load_project_config(workspace_root()).unwrap();
        let router = AdapterRouter::new(AdapterRegistry::from_project(&project).unwrap());
        let request =
            AdapterRouteRequest::new(CapabilityName::parse("workflow.code_review").unwrap())
                .with_provider(AdapterId::parse("code-review-skill").unwrap());

        let route = router.route(&request).unwrap();

        assert_eq!(route.handle.id.as_str(), "code-review-skill");
        assert_eq!(route.reason, "explicit provider");
    }
}
