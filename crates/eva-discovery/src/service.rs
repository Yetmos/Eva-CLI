//! 发现服务的协调入口。
//! Discovery service coordination.

use crate::cache::DiscoveryCache;
use crate::health::{DiscoveryHealth, DiscoveryHealthStatus};
use crate::normalizer::DiscoveryCandidate;
use crate::scanner::{scan_sources, DiscoveryScanReport, DiscoverySource, ProjectDiscoverySource};
use crate::sources::codex::CodexDiscoverySource;
use crate::sources::mcp::McpDiscoverySource;
use crate::sources::omx::OmxDiscoverySource;
use crate::sources::path_commands::PathCommandDiscoverySource;
use crate::sources::registry::ExternalRegistryDiscoverySource;
use eva_config::ProjectConfig;

/// 本模块的架构职责：协调可信来源发现，但不执行授权。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "coordinate trusted source discovery without authorization";

/// 聚合多种发现来源并维护最近快照的服务。
///
/// 该服务只负责枚举与诊断候选项。即使候选项来自允许列表，仍不会在此创建连接、
/// 启动进程或授予句柄。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiscoveryService {
    /// 最近一次完整或增量扫描产生的候选项缓存。
    cache: DiscoveryCache,
}

impl DiscoveryService {
    /// 创建尚未执行扫描的空服务。
    pub fn new() -> Self {
        Self::default()
    }

    /// 执行所有项目来源的完整扫描，并以本轮聚合结果替换缓存。
    pub fn scan_project(&mut self, project: &ProjectConfig) -> DiscoveryScanReport {
        let report = scan_project_sources(project);
        self.cache
            .replace(report.candidates.clone(), "project multi-source scan");
        report
    }

    /// 逐来源执行增量扫描，只把成功来源的新结果合入缓存。
    ///
    /// 来源报告包含错误时不会覆盖该来源的旧快照；这使暂时性扫描故障不会把之前
    /// 已知的候选项误删。返回报告的候选项随后改为完整缓存视图，而逐来源报告仍
    /// 精确描述本轮扫描结果。
    pub fn scan_project_incremental(&mut self, project: &ProjectConfig) -> DiscoveryScanReport {
        let mut report = scan_project_sources(project);
        for source_report in &report.source_reports {
            if source_report.error.is_none() {
                self.cache.merge_source(
                    &source_report.cache_key,
                    source_report.candidates.clone(),
                    format!("incremental scan: {}", source_report.cache_key),
                );
            }
        }
        report.candidates = self.cache.snapshot().to_vec();
        report
    }

    /// 借用底层缓存以读取快照元数据。
    pub fn cache(&self) -> &DiscoveryCache {
        &self.cache
    }

    /// 返回当前规范化候选项快照。
    pub fn candidates(&self) -> &[DiscoveryCandidate] {
        self.cache.snapshot()
    }

    /// 将当前候选项映射为无副作用的健康记录。
    ///
    /// 没有拒绝原因只表示候选项已被接受为“可见”，提示消息会再次强调授权仍由
    /// 外部边界负责，避免健康状态被误解为可执行性证明。
    pub fn health(&self) -> Vec<DiscoveryHealth> {
        self.cache
            .snapshot()
            .iter()
            .map(|candidate| DiscoveryHealth {
                candidate_id: candidate.id.clone(),
                status: if candidate.rejected_reason.is_some() {
                    DiscoveryHealthStatus::Rejected
                } else {
                    DiscoveryHealthStatus::Seen
                },
                message: candidate.rejected_reason.clone().unwrap_or_else(|| {
                    "candidate discovered; authorization remains external".to_owned()
                }),
            })
            .collect()
    }
}

/// 构造固定顺序的内置来源并执行一次统一扫描。
///
/// 固定顺序配合候选项去重规则保证报告稳定；各来源仍独立产出状态，单个来源失败
/// 不会阻止后续来源继续扫描。
fn scan_project_sources(project: &ProjectConfig) -> DiscoveryScanReport {
    let project_source = ProjectDiscoverySource::new(project);
    let path_source = PathCommandDiscoverySource::new(project);
    let mcp_source = McpDiscoverySource::new(project);
    let omx_source = OmxDiscoverySource::new(project);
    let codex_source = CodexDiscoverySource::new(project);
    let registry_source = ExternalRegistryDiscoverySource::new(project);
    scan_sources(&[
        &project_source as &dyn DiscoverySource,
        &path_source as &dyn DiscoverySource,
        &mcp_source as &dyn DiscoverySource,
        &omx_source as &dyn DiscoverySource,
        &codex_source as &dyn DiscoverySource,
        &registry_source as &dyn DiscoverySource,
    ])
}

#[cfg(test)]
/// 发现服务缓存、增量更新和健康映射的集成测试。
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use std::path::{Path, PathBuf};

    /// 返回用于加载真实项目配置的工作区根目录。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    /// 验证完整扫描缓存候选项，但不赋予任何运行时句柄。
    fn service_caches_candidates_without_handles() {
        let project = load_project_config(workspace_root()).unwrap();
        let mut service = DiscoveryService::new();

        let report = service.scan_project(&project);

        assert_eq!(service.candidates().len(), report.candidates.len());
        assert!(service
            .candidates()
            .iter()
            .all(|candidate| !candidate.handle_granted));
        assert!(report
            .source_reports
            .iter()
            .any(|source| source.source_id == "path_commands"));
        assert!(report
            .source_reports
            .iter()
            .any(|source| source.source_id == "mcp"));
        assert!(report
            .source_reports
            .iter()
            .any(|source| source.source_id == "codex"));
    }

    #[test]
    /// 验证重复增量扫描按来源更新并保持无授权语义。
    fn incremental_scan_updates_sources_without_granting_handles() {
        let project = load_project_config(workspace_root()).unwrap();
        let mut service = DiscoveryService::new();

        let first = service.scan_project_incremental(&project);
        let second = service.scan_project_incremental(&project);

        assert_eq!(service.candidates().len(), second.candidates.len());
        assert_eq!(first.source_reports.len(), second.source_reports.len());
        assert!(service
            .cache()
            .refresh_reason()
            .unwrap()
            .starts_with("incremental scan:"));
        assert!(service
            .candidates()
            .iter()
            .all(|candidate| !candidate.handle_granted));
    }

    #[test]
    /// 验证被来源拒绝的候选项仍可通过健康记录观察原因。
    fn rejected_source_candidates_are_visible_in_health() {
        let project = load_project_config(workspace_root()).unwrap();
        let mut service = DiscoveryService::new();

        service.scan_project_incremental(&project);
        let health = service.health();

        assert!(health.iter().any(|entry| {
            entry.status == DiscoveryHealthStatus::Rejected
                && entry
                    .message
                    .contains("external registry source is not configured")
        }));
    }
}
