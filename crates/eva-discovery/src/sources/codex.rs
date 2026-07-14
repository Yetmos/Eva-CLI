//! 从可信本地适配器清单发现 Codex 工作流入口。
//! Codex workflow surface discovery from trusted local Adapter manifests.

use crate::normalizer::{DiscoveryCandidate, DiscoveryCandidateKind, DiscoveryTrust};
use crate::scanner::DiscoverySource;
use eva_config::ProjectConfig;
use eva_core::EvaError;

/// 本来源的架构职责：发现可信的 Codex 能力描述。
pub const RESPONSIBILITY: &str = "discover trusted Codex capabilities";

/// 基于项目配置的 Codex 发现来源。
pub struct CodexDiscoverySource<'a> {
    /// 只读项目配置；扫描不会修改清单或启动 Codex。
    project: &'a ProjectConfig,
}

impl<'a> CodexDiscoverySource<'a> {
    /// 为指定项目配置创建发现来源。
    pub fn new(project: &'a ProjectConfig) -> Self {
        Self { project }
    }
}

impl DiscoverySource for CodexDiscoverySource<'_> {
    /// 返回用于报告和增量缓存的稳定来源标识。
    fn source_id(&self) -> &str {
        "codex"
    }

    /// 返回本地清单扫描允许的最大耗时。
    fn timeout_ms(&self) -> u64 {
        250
    }

    /// 枚举清单中名称或命令指向 Codex 的适配器。
    ///
    /// 禁用的适配器仍作为被拒绝候选项返回，以便诊断配置；扫描只读取元数据，
    /// 不验证或执行命令。
    fn scan(&self) -> Result<Vec<DiscoveryCandidate>, EvaError> {
        let mut candidates = Vec::new();
        for adapter in &self.project.adapters {
            let command = adapter.extra_string("command");
            let is_codex_adapter =
                adapter.id.as_str().contains("codex") || command == Some("codex");
            if !is_codex_adapter {
                continue;
            }
            let mut candidate = DiscoveryCandidate::named(
                self.source_id(),
                DiscoveryCandidateKind::Workflow,
                "codex-cli",
                Some(adapter.id.clone()),
                DiscoveryTrust::ConfiguredAllowlist,
            );
            if !adapter.enabled {
                candidate = candidate.rejected("Codex adapter manifest is disabled");
            }
            candidates.push(candidate);
        }
        Ok(candidates)
    }
}
