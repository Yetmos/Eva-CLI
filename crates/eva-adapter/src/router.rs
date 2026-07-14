//! 本模块提供 `router` 相关实现。
//! Adapter provider selection by explicit provider or capability index.

use crate::manifest::AdapterHandle;
use crate::registry::AdapterRegistry;
use eva_core::{AdapterId, CapabilityName, EvaError};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "provider selection by explicit provider or capability";

/// 表示 `AdapterRouteRequest` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterRouteRequest {
    /// 记录 `capability` 字段对应的值。
    pub capability: CapabilityName,
    /// 记录 `provider` 字段对应的值。
    pub provider: Option<AdapterId>,
}

/// 表示 `AdapterRoute` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterRoute {
    /// 记录 `handle` 字段对应的值。
    pub handle: AdapterHandle,
    /// 记录 `reason` 字段对应的值。
    pub reason: String,
}

/// 表示 `AdapterRouter` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterRouter {
    /// 记录 `registry` 字段对应的值。
    registry: AdapterRegistry,
}

impl AdapterRouteRequest {
    /// 创建并初始化当前类型的实例。
    pub fn new(capability: CapabilityName) -> Self {
        Self {
            capability,
            provider: None,
        }
    }

    /// 设置 `provider` 并返回更新后的实例。
    pub fn with_provider(mut self, provider: AdapterId) -> Self {
        self.provider = Some(provider);
        self
    }
}

impl AdapterRouter {
    /// 创建并初始化当前类型的实例。
    pub fn new(registry: AdapterRegistry) -> Self {
        Self { registry }
    }

    /// 执行 `registry` 对应的处理逻辑。
    pub fn registry(&self) -> &AdapterRegistry {
        &self.registry
    }

    /// 执行 `route` 对应的受控流程。
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

    /// 执行 `route_handle` 对应的受控流程。
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

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use std::path::{Path, PathBuf};

    /// 执行 `workspace_root` 对应的处理逻辑。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    /// 验证 `router_uses_explicit_provider_when_present` 场景下的预期行为。
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
