//! 外部注册表的发现边界。
//! External registry discovery boundary.

use crate::normalizer::{DiscoveryCandidate, DiscoveryCandidateKind, DiscoveryTrust};
use crate::scanner::{DiscoveryScanContext, DiscoverySource};
use eva_config::ProjectConfig;
use eva_core::EvaError;

/// 本来源的架构职责：发现已显式配置的外部注册表候选项。
pub const RESPONSIBILITY: &str = "discover configured external registry candidates";

/// 通过本地配置标记探测外部注册表可见性的来源。
pub struct ExternalRegistryDiscoverySource<'a> {
    /// 用于定位注册表配置目录的只读项目配置。
    project: &'a ProjectConfig,
}

impl<'a> ExternalRegistryDiscoverySource<'a> {
    /// 为指定项目配置创建发现来源。
    pub fn new(project: &'a ProjectConfig) -> Self {
        Self { project }
    }
}

impl DiscoverySource for ExternalRegistryDiscoverySource<'_> {
    /// 返回用于报告和增量缓存的稳定来源标识。
    fn source_id(&self) -> &str {
        "external_registry"
    }

    /// 返回本地配置探测允许的最大耗时。
    fn timeout_ms(&self) -> u64 {
        100
    }

    /// 根据注册表配置目录是否存在生成展示级候选项。
    ///
    /// 本边界不会访问网络，也不会把目录存在视为授权；未配置时保留拒绝候选项，
    /// 使调用方可以区分“未配置”和“扫描未运行”。
    fn scan(&self, context: &DiscoveryScanContext) -> Result<Vec<DiscoveryCandidate>, EvaError> {
        context.check()?;
        let registry_marker = self.project.project_root.join("config/registries");
        let mut candidate = DiscoveryCandidate::named(
            self.source_id(),
            DiscoveryCandidateKind::RegistryEntry,
            "configured-registry",
            None,
            DiscoveryTrust::DisplayOnly,
        );
        if !registry_marker.is_dir() {
            candidate = candidate.rejected("external registry source is not configured");
        }
        Ok(vec![candidate])
    }
}
