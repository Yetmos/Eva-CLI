//! 从本地工作区状态发现 OMX 工作流。
//! OMX workflow discovery from local workspace state.

use crate::normalizer::{DiscoveryCandidate, DiscoveryCandidateKind, DiscoveryTrust};
use crate::scanner::DiscoverySource;
use eva_config::ProjectConfig;
use eva_core::EvaError;

/// 本来源的架构职责：发现可信的 OMX 工作流入口。
pub const RESPONSIBILITY: &str = "discover trusted OMX workflow surfaces";

/// 根据项目根目录下 `.omx` 状态目录判断 OMX 可见性的来源。
pub struct OmxDiscoverySource<'a> {
    /// 用于定位工作区状态目录的只读项目配置。
    project: &'a ProjectConfig,
}

impl<'a> OmxDiscoverySource<'a> {
    /// 为指定项目配置创建发现来源。
    pub fn new(project: &'a ProjectConfig) -> Self {
        Self { project }
    }
}

impl DiscoverySource for OmxDiscoverySource<'_> {
    /// 返回用于报告和增量缓存的稳定来源标识。
    fn source_id(&self) -> &str {
        "omx"
    }

    /// 返回本地目录探测允许的最大耗时。
    fn timeout_ms(&self) -> u64 {
        100
    }

    /// 根据 `.omx` 目录是否存在生成工作流候选项。
    ///
    /// 目录不存在时仍返回展示级候选项及拒绝原因，目录存在也只提升来源信任等级，
    /// 不读取 OMX 状态、不恢复会话，也不授予执行权限。
    fn scan(&self) -> Result<Vec<DiscoveryCandidate>, EvaError> {
        let omx_root = self.project.project_root.join(".omx");
        let trust = if omx_root.is_dir() {
            DiscoveryTrust::ConfiguredAllowlist
        } else {
            DiscoveryTrust::DisplayOnly
        };
        let mut candidate = DiscoveryCandidate::named(
            self.source_id(),
            DiscoveryCandidateKind::Workflow,
            "omx-workspace",
            None,
            trust,
        );
        if !omx_root.is_dir() {
            candidate = candidate.rejected(".omx state directory is not present");
        }
        Ok(vec![candidate])
    }
}
