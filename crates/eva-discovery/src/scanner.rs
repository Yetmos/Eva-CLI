//! 发现来源的统一编排与报告。
//! Discovery source orchestration.

use crate::normalizer::{dedupe, DiscoveryCandidate, DiscoveryCandidateKind, DiscoveryTrust};
use eva_config::{AdapterTransport, CapabilityKind, ProjectConfig};
use eva_core::EvaError;
use std::collections::BTreeSet;
use std::time::Instant;

/// 本模块的架构职责：扫描可信来源并产出候选能力及逐来源诊断。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "scan trusted sources for candidate capabilities";

/// 可被统一编排的发现来源接口。
///
/// 实现应只枚举候选描述，不授予句柄或执行候选能力。编排器会隔离每个来源的错误
/// 并记录耗时，但 `scan` 本身是同步调用，超时预算用于事后拒绝超时结果，而非抢占。
pub trait DiscoverySource {
    /// 返回稳定、可读的来源标识。
    fn source_id(&self) -> &str;
    /// 返回增量缓存分区键；默认与来源标识一致。
    fn cache_key(&self) -> String {
        self.source_id().to_owned()
    }
    /// 返回允许的扫描耗时，单位为毫秒。
    fn timeout_ms(&self) -> u64 {
        1_000
    }
    /// 扫描来源并返回不携带运行时授权的候选项。
    fn scan(&self) -> Result<Vec<DiscoveryCandidate>, EvaError>;
}

/// 单个发现来源的一次扫描报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverySourceReport {
    /// 来源的稳定标识。
    pub source_id: String,
    /// 用于增量缓存替换的分区键。
    pub cache_key: String,
    /// 来源声明的耗时预算，单位为毫秒。
    pub timeout_ms: u64,
    /// 实际观测到的扫描耗时，单位为毫秒。
    pub elapsed_ms: u64,
    /// `ok`、`rejected`、`timeout` 或 `error` 状态。
    pub status: String,
    /// 本来源在预算内成功产出的候选项。
    pub candidates: Vec<DiscoveryCandidate>,
    /// 扫描失败或超时时的错误信息。
    pub error: Option<String>,
    /// 来源整体被拒绝时汇总的稳定原因。
    pub rejected_reason: Option<String>,
}

/// 一轮多来源扫描的聚合结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryScanReport {
    /// 所有成功且未超时来源的去重候选项。
    pub candidates: Vec<DiscoveryCandidate>,
    /// 按输入来源顺序保存的独立诊断报告。
    pub source_reports: Vec<DiscoverySourceReport>,
}

/// 从项目清单直接发现适配器与能力的基础来源。
pub struct ProjectDiscoverySource<'a> {
    /// 被扫描的只读项目配置。
    project: &'a ProjectConfig,
}

impl<'a> ProjectDiscoverySource<'a> {
    /// 为指定项目配置创建基础发现来源。
    pub fn new(project: &'a ProjectConfig) -> Self {
        Self { project }
    }
}

impl DiscoverySource for ProjectDiscoverySource<'_> {
    /// 返回项目清单来源的稳定标识。
    fn source_id(&self) -> &str {
        "project_config"
    }

    /// 将适配器及顶层能力清单规范化为候选项。
    ///
    /// 适配器能力依据传输类型细分为 MCP 工具、技能或通用能力；禁用适配器仍会
    /// 作为展示级拒绝项出现。顶层能力来自显式配置允许列表，但同样不获得句柄。
    fn scan(&self) -> Result<Vec<DiscoveryCandidate>, EvaError> {
        let mut candidates = Vec::new();
        for adapter in &self.project.adapters {
            candidates.push(DiscoveryCandidate::adapter(
                self.source_id(),
                adapter.id.clone(),
            ));
            for capability in &adapter.capabilities {
                let kind = match adapter.transport {
                    AdapterTransport::Mcp => DiscoveryCandidateKind::McpTool,
                    AdapterTransport::Skill => DiscoveryCandidateKind::Skill,
                    _ => DiscoveryCandidateKind::Capability,
                };
                let trust = if adapter.enabled {
                    DiscoveryTrust::ProjectManifest
                } else {
                    DiscoveryTrust::DisplayOnly
                };
                let candidate = DiscoveryCandidate::capability(
                    self.source_id(),
                    capability.clone(),
                    Some(adapter.id.clone()),
                    kind,
                    trust,
                );
                candidates.push(if adapter.enabled {
                    candidate
                } else {
                    candidate.rejected("adapter manifest is disabled")
                });
            }
        }
        for capability in &self.project.capabilities {
            let kind = match capability.kind {
                CapabilityKind::McpTool => DiscoveryCandidateKind::McpTool,
                CapabilityKind::Skill => DiscoveryCandidateKind::Skill,
                _ => DiscoveryCandidateKind::Capability,
            };
            candidates.push(DiscoveryCandidate::capability(
                self.source_id(),
                capability.capability.clone(),
                capability
                    .default_provider
                    .clone()
                    .or_else(|| capability.provider.clone()),
                kind,
                DiscoveryTrust::ConfiguredAllowlist,
            ));
        }
        Ok(candidates)
    }
}

/// 依次扫描来源、隔离失败并生成确定性的聚合报告。
///
/// `timeout_ms == 0` 会在调用来源前直接拒绝；其他超时在同步扫描返回后判断，超时
/// 来源产出的候选项会被全部丢弃，防止过期或失控来源污染聚合快照。某来源返回
/// 错误只记录在该来源报告中，后续来源仍会继续执行。
pub fn scan_sources(sources: &[&dyn DiscoverySource]) -> DiscoveryScanReport {
    let mut all = Vec::new();
    let mut source_reports = Vec::new();
    for source in sources {
        let cache_key = source.cache_key();
        let timeout_ms = source.timeout_ms();
        // 零预算明确表示来源不可运行，必须在触发任何来源副作用前短路。
        if timeout_ms == 0 {
            source_reports.push(DiscoverySourceReport {
                source_id: source.source_id().to_owned(),
                cache_key,
                timeout_ms,
                elapsed_ms: 0,
                status: "timeout".to_owned(),
                candidates: Vec::new(),
                error: Some("discovery source timed out before scan".to_owned()),
                rejected_reason: Some("timeout_ms is zero".to_owned()),
            });
            continue;
        }
        let started = Instant::now();
        match source.scan() {
            Ok(candidates) => {
                let elapsed_ms = elapsed_ms(started);
                // 同步接口无法抢占扫描，因此返回后再执行预算检查；超时结果不可合入。
                if elapsed_ms > timeout_ms {
                    source_reports.push(DiscoverySourceReport {
                        source_id: source.source_id().to_owned(),
                        cache_key,
                        timeout_ms,
                        elapsed_ms,
                        status: "timeout".to_owned(),
                        candidates: Vec::new(),
                        error: Some("discovery source timed out".to_owned()),
                        rejected_reason: Some(format!(
                            "elapsed_ms {elapsed_ms} exceeded timeout_ms {timeout_ms}"
                        )),
                    });
                    continue;
                }
                let status = source_status(&candidates).to_owned();
                let rejected_reason = source_rejected_reason(&candidates);
                all.extend(candidates.clone());
                source_reports.push(DiscoverySourceReport {
                    source_id: source.source_id().to_owned(),
                    cache_key,
                    timeout_ms,
                    elapsed_ms,
                    status,
                    candidates,
                    error: None,
                    rejected_reason,
                });
            }
            Err(error) => source_reports.push(DiscoverySourceReport {
                source_id: source.source_id().to_owned(),
                cache_key,
                timeout_ms,
                elapsed_ms: elapsed_ms(started),
                status: "error".to_owned(),
                candidates: Vec::new(),
                error: Some(error.message().to_owned()),
                rejected_reason: Some(error.message().to_owned()),
            }),
        }
    }
    DiscoveryScanReport {
        candidates: dedupe(all),
        source_reports,
    }
}

/// 将扫描起点到当前时刻的耗时安全转换为毫秒。
///
/// 极端情况下若毫秒数超过 `u64`，使用最大值以保证后续预算比较保守地判为超时。
fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

/// 根据候选项拒绝情况计算来源级状态。
fn source_status(candidates: &[DiscoveryCandidate]) -> &'static str {
    if !candidates.is_empty()
        && candidates
            .iter()
            .all(|candidate| candidate.rejected_reason.is_some())
    {
        "rejected"
    } else {
        "ok"
    }
}

/// 去重并汇总来源内所有候选项的拒绝原因。
fn source_rejected_reason(candidates: &[DiscoveryCandidate]) -> Option<String> {
    let reasons = candidates
        .iter()
        .filter_map(|candidate| candidate.rejected_reason.as_deref())
        .collect::<BTreeSet<_>>();
    if reasons.is_empty() {
        None
    } else {
        Some(reasons.into_iter().collect::<Vec<_>>().join("; "))
    }
}

#[cfg(test)]
/// 多来源扫描的候选映射、超时和拒绝语义测试。
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use std::path::{Path, PathBuf};

    /// 返回用于加载真实项目配置的工作区根目录。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    /// 验证项目清单来源能识别 MCP 与技能能力且不授予句柄。
    fn project_source_finds_mcp_and_skill_candidates() {
        let project = load_project_config(workspace_root()).unwrap();
        let source = ProjectDiscoverySource::new(&project);
        let candidates = source.scan().unwrap();

        assert!(candidates
            .iter()
            .any(|candidate| candidate.kind == DiscoveryCandidateKind::McpTool));
        assert!(candidates
            .iter()
            .any(|candidate| candidate.kind == DiscoveryCandidateKind::Skill));
        assert!(candidates.iter().all(|candidate| !candidate.handle_granted));
    }

    /// 用零耗时预算验证扫描前超时短路的测试来源。
    struct TimeoutSource;

    impl DiscoverySource for TimeoutSource {
        /// 返回测试来源标识。
        fn source_id(&self) -> &str {
            "slow_source"
        }

        /// 返回零预算，使编排器不得调用 `scan`。
        fn timeout_ms(&self) -> u64 {
            0
        }

        /// 若被误调用则返回可识别候选项，用于暴露短路失效。
        fn scan(&self) -> Result<Vec<DiscoveryCandidate>, EvaError> {
            Ok(vec![DiscoveryCandidate::named(
                self.source_id(),
                DiscoveryCandidateKind::Workflow,
                "should-not-run",
                None,
                DiscoveryTrust::ConfiguredAllowlist,
            )])
        }
    }

    #[test]
    /// 验证零预算来源不执行扫描且不会贡献候选项。
    fn scan_sources_reports_timeout_without_candidates() {
        let source = TimeoutSource;
        let report = scan_sources(&[&source]);

        assert!(report.candidates.is_empty());
        assert_eq!(report.source_reports[0].status, "timeout");
        assert_eq!(
            report.source_reports[0].rejected_reason.as_deref(),
            Some("timeout_ms is zero")
        );
    }

    /// 始终返回被拒绝候选项的测试来源。
    struct RejectedSource;

    impl DiscoverySource for RejectedSource {
        /// 返回测试来源标识。
        fn source_id(&self) -> &str {
            "rejected_source"
        }

        /// 返回带明确拒绝原因的候选项。
        fn scan(&self) -> Result<Vec<DiscoveryCandidate>, EvaError> {
            Ok(vec![DiscoveryCandidate::named(
                self.source_id(),
                DiscoveryCandidateKind::Workflow,
                "disabled-workflow",
                None,
                DiscoveryTrust::DisplayOnly,
            )
            .rejected("workflow is disabled")])
        }
    }

    #[test]
    /// 验证来源级报告会汇总候选项拒绝原因。
    fn scan_sources_reports_candidate_rejections() {
        let source = RejectedSource;
        let report = scan_sources(&[&source]);

        assert_eq!(report.source_reports[0].status, "rejected");
        assert_eq!(
            report.source_reports[0].rejected_reason.as_deref(),
            Some("workflow is disabled")
        );
        assert!(report
            .candidates
            .iter()
            .all(|candidate| !candidate.handle_granted));
    }
}
