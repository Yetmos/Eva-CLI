//! 发现候选项的规范化模型。
//! Discovery candidate normalization.

use eva_core::{AdapterId, CapabilityName};

/// 本模块的架构职责：规范化候选项及其拒绝原因。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "normalize discovered candidates and rejected reasons";

/// 发现候选项的业务类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiscoveryCandidateKind {
    /// 可被调度的 Agent 描述。
    Agent,
    /// 项目中声明的适配器。
    Adapter,
    /// 不属于更具体类别的通用能力。
    Capability,
    /// 来自适配器清单的 PATH 命令名。
    PathCommand,
    /// MCP 服务暴露且被允许的工具。
    McpTool,
    /// 技能能力。
    Skill,
    /// Codex、OMX 等工作流入口。
    Workflow,
    /// 外部注册表中的描述性条目。
    RegistryEntry,
}

/// 候选项来源所能提供的信任等级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiscoveryTrust {
    /// 候选项由当前项目清单直接声明。
    ProjectManifest,
    /// 候选项位于显式配置的允许列表中。
    ConfiguredAllowlist,
    /// 候选项只允许展示，不能据此获得执行权限。
    DisplayOnly,
}

/// 不携带运行时权限的规范化发现候选项。
///
/// `trust` 只描述来源可信度，`handle_granted` 在发现层始终为 `false`。实际授权必须
/// 由发现层之外的策略与运行时边界完成，调用方不得把“发现到”解释为“可执行”。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryCandidate {
    /// 由类别、可选适配器和名称组成的稳定去重标识。
    pub id: String,
    /// 候选项的业务类别。
    pub kind: DiscoveryCandidateKind,
    /// 产生候选项的来源标识。
    pub source: String,
    /// 来源能够证明的信任等级。
    pub trust: DiscoveryTrust,
    /// 候选项关联的适配器；全局候选项为 `None`。
    pub adapter_id: Option<AdapterId>,
    /// 候选项关联的结构化能力名；命名候选项为 `None`。
    pub capability: Option<CapabilityName>,
    /// 是否已授予运行时句柄；发现边界构造的候选项固定为 `false`。
    pub handle_granted: bool,
    /// 候选项仅可见但不可接受时的具体原因。
    pub rejected_reason: Option<String>,
}

impl DiscoveryCandidateKind {
    /// 返回用于构造稳定标识和报告的类别字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Adapter => "adapter",
            Self::Capability => "capability",
            Self::PathCommand => "path_command",
            Self::McpTool => "mcp_tool",
            Self::Skill => "skill",
            Self::Workflow => "workflow",
            Self::RegistryEntry => "registry_entry",
        }
    }
}

impl DiscoveryTrust {
    /// 返回用于报告和序列化展示的稳定信任级别字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProjectManifest => "project_manifest",
            Self::ConfiguredAllowlist => "configured_allowlist",
            Self::DisplayOnly => "display_only",
        }
    }
}

impl DiscoveryCandidate {
    /// 从项目适配器清单构造候选项，且不授予运行时句柄。
    pub fn adapter(source: impl Into<String>, adapter_id: AdapterId) -> Self {
        Self {
            id: format!("adapter:{}", adapter_id.as_str()),
            kind: DiscoveryCandidateKind::Adapter,
            source: source.into(),
            trust: DiscoveryTrust::ProjectManifest,
            adapter_id: Some(adapter_id),
            capability: None,
            handle_granted: false,
            rejected_reason: None,
        }
    }

    /// 从结构化能力构造候选项，并按是否关联适配器生成稳定标识。
    pub fn capability(
        source: impl Into<String>,
        capability: CapabilityName,
        adapter_id: Option<AdapterId>,
        kind: DiscoveryCandidateKind,
        trust: DiscoveryTrust,
    ) -> Self {
        // 标识包含适配器维度，避免两个适配器提供同名能力时发生错误合并。
        let id = match &adapter_id {
            Some(adapter_id) => format!(
                "{}:{}:{}",
                kind.as_str(),
                adapter_id.as_str(),
                capability.as_str()
            ),
            None => format!("{}:{}", kind.as_str(), capability.as_str()),
        };
        Self {
            id,
            kind,
            source: source.into(),
            trust,
            adapter_id,
            capability: Some(capability),
            handle_granted: false,
            rejected_reason: None,
        }
    }

    /// 从普通名称构造非结构化候选项。
    pub fn named(
        source: impl Into<String>,
        kind: DiscoveryCandidateKind,
        name: impl Into<String>,
        adapter_id: Option<AdapterId>,
        trust: DiscoveryTrust,
    ) -> Self {
        let source = source.into();
        let name = name.into();
        // 来源不参与标识；同一逻辑候选项由多个来源发现时，后续去重只保留一份。
        let id = match &adapter_id {
            Some(adapter_id) => format!("{}:{}:{}", kind.as_str(), adapter_id.as_str(), name),
            None => format!("{}:{}", kind.as_str(), name),
        };
        Self {
            id,
            kind,
            source,
            trust,
            adapter_id,
            capability: None,
            handle_granted: false,
            rejected_reason: None,
        }
    }

    /// 为候选项附加拒绝原因，同时保留候选项供诊断和健康展示。
    pub fn rejected(mut self, reason: impl Into<String>) -> Self {
        self.rejected_reason = Some(reason.into());
        self
    }
}

/// 按稳定标识合并重复候选项，并保证输出顺序确定。
///
/// 排序时以来源作为次级键，因此同一标识来自多个来源时，保留来源字典序最小的
/// 记录。该规则使重复扫描输出可复现，但不会提升任何候选项的信任或授权等级。
pub fn dedupe(mut candidates: Vec<DiscoveryCandidate>) -> Vec<DiscoveryCandidate> {
    candidates.sort_by(|left, right| left.id.cmp(&right.id).then(left.source.cmp(&right.source)));
    candidates.dedup_by(|left, right| left.id == right.id);
    candidates
}

#[cfg(test)]
/// 规范化候选项的边界行为测试。
mod tests {
    use super::*;

    #[test]
    /// 验证发现层构造器永远不会隐式授予运行时句柄。
    fn candidates_never_grant_runtime_handles() {
        let candidate = DiscoveryCandidate::adapter(
            "project_adapters",
            AdapterId::parse("github-mcp").unwrap(),
        );

        assert!(!candidate.handle_granted);
    }
}
