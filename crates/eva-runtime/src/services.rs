//! Runtime service summaries for the current generation.

use eva_config::ProjectConfig;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "hold wired service handles for the runtime generation";

/// Stable state label for a runtime service boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Ready,
    Planned,
    Disabled,
}

/// Read-only service summary exposed to CLI inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSummary {
    pub name: String,
    pub state: ServiceState,
    pub detail: String,
}

/// V0.3 service container. It intentionally stores summaries only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeServices {
    summaries: Vec<ServiceSummary>,
}

impl ServiceState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Planned => "planned",
            Self::Disabled => "disabled",
        }
    }
}

impl ServiceSummary {
    pub fn new(name: impl Into<String>, state: ServiceState, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            state,
            detail: detail.into(),
        }
    }
}

impl RuntimeServices {
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

    pub fn summaries(&self) -> &[ServiceSummary] {
        &self.summaries
    }

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
