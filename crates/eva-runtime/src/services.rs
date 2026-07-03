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
}
