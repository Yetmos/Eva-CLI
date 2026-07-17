//! 从已配置的适配器清单发现 MCP 工具。
//! MCP tool discovery from configured Adapter manifests.

use crate::normalizer::{DiscoveryCandidate, DiscoveryCandidateKind, DiscoveryTrust};
use crate::scanner::{DiscoveryScanContext, DiscoverySource};
use eva_config::{AdapterTransport, ProjectConfig};
use eva_core::EvaError;

/// 本来源的架构职责：发现清单中显式配置的 MCP 服务能力。
pub const RESPONSIBILITY: &str = "discover configured MCP server capabilities";

/// 基于项目适配器配置的 MCP 工具发现来源。
pub struct McpDiscoverySource<'a> {
    /// 只读项目配置；扫描不会连接 MCP 服务。
    project: &'a ProjectConfig,
}

impl<'a> McpDiscoverySource<'a> {
    /// 为指定项目配置创建发现来源。
    pub fn new(project: &'a ProjectConfig) -> Self {
        Self { project }
    }
}

impl DiscoverySource for McpDiscoverySource<'_> {
    /// 返回用于报告和增量缓存的稳定来源标识。
    fn source_id(&self) -> &str {
        "mcp"
    }

    /// 返回本地清单扫描允许的最大耗时。
    fn timeout_ms(&self) -> u64 {
        250
    }

    /// 仅从 MCP 适配器的工具允许列表构造候选项。
    ///
    /// 空允许列表和禁用适配器都会产生带拒绝原因的可见候选项；本方法不会向服务
    /// 请求工具列表，因此配置扫描不会意外扩大网络或执行边界。
    fn scan(&self, context: &DiscoveryScanContext) -> Result<Vec<DiscoveryCandidate>, EvaError> {
        context.check()?;
        let mut candidates = Vec::new();
        for adapter in &self.project.adapters {
            if adapter.transport != AdapterTransport::Mcp {
                continue;
            }
            let tools = adapter.nested_extra_string_list("mcp", "tool_allowlist");
            if tools.is_empty() {
                candidates.push(
                    DiscoveryCandidate::named(
                        self.source_id(),
                        DiscoveryCandidateKind::McpTool,
                        "tool_allowlist",
                        Some(adapter.id.clone()),
                        DiscoveryTrust::DisplayOnly,
                    )
                    .rejected("MCP adapter has no tool allowlist"),
                );
                continue;
            }
            for tool in tools {
                let mut candidate = DiscoveryCandidate::named(
                    self.source_id(),
                    DiscoveryCandidateKind::McpTool,
                    tool,
                    Some(adapter.id.clone()),
                    DiscoveryTrust::ConfiguredAllowlist,
                );
                if !adapter.enabled {
                    candidate = candidate.rejected("adapter manifest is disabled");
                }
                candidates.push(candidate);
            }
        }
        Ok(candidates)
    }
}
