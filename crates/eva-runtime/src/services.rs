//! 中文：当前代际的运行时服务状态摘要和模式装配表。
//! Runtime service summaries for the current generation.

use eva_config::ProjectConfig;

/// 中文：本模块保存当前代际已就绪、规划中或关闭的服务边界状态。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "hold wired service handles for the runtime generation";

/// 中文：单个运行时服务边界的稳定状态标签。
/// Stable state label for a runtime service boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    /// 中文：服务已在当前模式中完成装配并可用。
    Ready,
    /// 中文：服务属于后续版本范围，当前模式尚未装配。
    Planned,
    /// 中文：服务被配置或策略明确关闭。
    Disabled,
}

/// 中文：向 CLI 检查命令暴露的只读服务摘要。
/// Read-only service summary exposed to CLI inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSummary {
    /// 中文：稳定服务边界名称。
    pub name: String,
    /// 中文：当前代际中的服务状态。
    pub state: ServiceState,
    /// 中文：面向操作员的状态细节。
    pub detail: String,
}

/// 中文：只保存摘要、不拥有真实服务句柄的轻量服务容器。
/// V0.3 service container. It intentionally stores summaries only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeServices {
    /// 中文：按展示顺序排列的服务摘要。
    summaries: Vec<ServiceSummary>,
}

impl ServiceState {
    /// 中文：返回用于 CLI 和 JSON 输出的稳定状态名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Planned => "planned",
            Self::Disabled => "disabled",
        }
    }
}

impl ServiceSummary {
    /// 中文：从名称、状态和说明创建服务摘要。
    pub fn new(name: impl Into<String>, state: ServiceState, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            state,
            detail: detail.into(),
        }
    }
}

impl RuntimeServices {
    /// 中文：构建 V0.3 Noop 服务表，仅配置、策略和可观察性边界为就绪。
    /// Builds the V0.3 no-op service table.
    pub fn noop(project: &ProjectConfig) -> Self {
        let summaries = vec![
            ServiceSummary::new(
                "config",
                ServiceState::Ready,
                format!(
                    "{} agents, {} routes",
                    project.agents.len(),
                    project.routes.routes.len()
                ),
            ),
            ServiceSummary::new(
                "policy",
                ServiceState::Ready,
                format!("{} policy documents loaded", project.policies.len()),
            ),
            ServiceSummary::new(
                "observability",
                ServiceState::Ready,
                "trace/audit/metric contracts available",
            ),
            ServiceSummary::new(
                "storage",
                ServiceState::Planned,
                "V0.4 in-memory/local traits",
            ),
            ServiceSummary::new("eventbus", ServiceState::Planned, "V0.4 in-memory bus"),
            ServiceSummary::new("scheduler", ServiceState::Planned, "V0.4 topic routing"),
            ServiceSummary::new(
                "agent_runtime",
                ServiceState::Planned,
                "V0.4 mailbox consumer",
            ),
            ServiceSummary::new("lua_host", ServiceState::Planned, "V0.4 sandboxed on_event"),
            ServiceSummary::new(
                "capability_router",
                ServiceState::Planned,
                "V0.4 builtin capability",
            ),
            ServiceSummary::new(
                "adapter_router",
                ServiceState::Planned,
                "V1.1 adapter runtime",
            ),
            ServiceSummary::new("mcp", ServiceState::Planned, "V1.1 MCP client/server"),
            ServiceSummary::new("discovery", ServiceState::Planned, "V1.1 trusted scan"),
            ServiceSummary::new("memory", ServiceState::Planned, "V1.2 context services"),
            ServiceSummary::new("hardware", ServiceState::Planned, "V1.3 hardware adapter"),
            ServiceSummary::new(
                "backup_lifecycle",
                ServiceState::Planned,
                "V1.4 backup, snapshot, supervisor",
            ),
        ];

        Self { summaries }
    }

    /// 中文：返回服务摘要的只读切片，顺序与模式装配表一致。
    pub fn summaries(&self) -> &[ServiceSummary] {
        &self.summaries
    }

    /// 中文：构建 V0.4 内存服务表，把核心事件处理链标记为就绪。
    /// Builds the V0.4 in-memory service table.
    pub fn in_memory_v04(project: &ProjectConfig) -> Self {
        let summaries = vec![
            ServiceSummary::new(
                "config",
                ServiceState::Ready,
                format!(
                    "{} agents, {} routes",
                    project.agents.len(),
                    project.routes.routes.len()
                ),
            ),
            ServiceSummary::new(
                "policy",
                ServiceState::Ready,
                format!("{} policy documents loaded", project.policies.len()),
            ),
            ServiceSummary::new(
                "observability",
                ServiceState::Ready,
                "trace/audit/metric contracts available",
            ),
            ServiceSummary::new("storage", ServiceState::Ready, "V0.4 in-memory event log"),
            ServiceSummary::new("eventbus", ServiceState::Ready, "V0.4 in-memory bus"),
            ServiceSummary::new("scheduler", ServiceState::Ready, "V0.4 topic routing"),
            ServiceSummary::new(
                "agent_runtime",
                ServiceState::Ready,
                "V0.4 bounded Agent queue",
            ),
            ServiceSummary::new(
                "lua_host",
                ServiceState::Ready,
                "V0.4 controlled on_event contract",
            ),
            ServiceSummary::new(
                "capability_router",
                ServiceState::Ready,
                "V0.4 builtin capability router",
            ),
            ServiceSummary::new(
                "adapter_router",
                ServiceState::Planned,
                "V1.1 adapter runtime",
            ),
            ServiceSummary::new("mcp", ServiceState::Planned, "V1.1 MCP client/server"),
            ServiceSummary::new("discovery", ServiceState::Planned, "V1.1 trusted scan"),
            ServiceSummary::new("memory", ServiceState::Planned, "V1.2 context services"),
            ServiceSummary::new("hardware", ServiceState::Planned, "V1.3 hardware adapter"),
            ServiceSummary::new(
                "backup_lifecycle",
                ServiceState::Planned,
                "V1.4 backup, snapshot, supervisor",
            ),
        ];

        Self { summaries }
    }

    /// 中文：在 V0.4 表基础上加入任务、死信重放和热重载诊断服务。
    /// Builds the V0.5 in-memory service table.
    pub fn in_memory_v05(project: &ProjectConfig) -> Self {
        let mut services = Self::in_memory_v04(project);
        services.summaries.extend([
            ServiceSummary::new(
                "task_registry",
                ServiceState::Ready,
                "V0.5 local task status/log/cancel records",
            ),
            ServiceSummary::new(
                "dead_letter_replay",
                ServiceState::Ready,
                "V0.5 in-memory dead-letter query and replay report",
            ),
            ServiceSummary::new(
                "hot_reload_generation",
                ServiceState::Ready,
                "V0.5 Lua generation marker for basic runtime",
            ),
        ]);
        services
    }

    /// 中文：在 V0.5 表基础上加入 V1.0 核心发布面，并保留高级能力规划状态。
    /// Builds the V1.0 core release service table.
    pub fn in_memory_v10(project: &ProjectConfig) -> Self {
        let mut services = Self::in_memory_v05(project);
        services.summaries.extend([
            ServiceSummary::new(
                "release_core",
                ServiceState::Ready,
                "V1.0 quickstart, CI, release notes, and known limits are documented",
            ),
            ServiceSummary::new(
                "advanced_capabilities",
                ServiceState::Planned,
                "Adapter, MCP, discovery, memory, hardware, backup, and lifecycle remain post-1.0 scope",
            ),
        ]);
        services
    }
}
