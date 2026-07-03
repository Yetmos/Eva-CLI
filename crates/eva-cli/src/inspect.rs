//! CLI inspect reports for configuration and no-op runtime state.

use crate::run::{display_path, json_array, json_string};
use eva_config::ProjectConfig;
use eva_core::EvaError;
use eva_runtime::{RuntimeBuilder, RuntimeSummary, ServiceSummary};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "inspect validated configuration and V0.3 no-op runtime status";

/// Combined `eva inspect` report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectReport {
    pub project_root: String,
    pub eva_config_path: String,
    pub environment: String,
    pub hot_reload: bool,
    pub agents: Vec<AgentInspect>,
    pub adapters: Vec<AdapterInspect>,
    pub capabilities: Vec<CapabilityInspect>,
    pub routes: Vec<RouteInspect>,
    pub policies: Vec<PolicyInspect>,
    pub runtime: RuntimeInspect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInspect {
    pub id: String,
    pub enabled: bool,
    pub script: String,
    pub subscriptions: Vec<String>,
    pub parent: Option<String>,
    pub children: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterInspect {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub transport: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityInspect {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub kind: String,
    pub capability: String,
    pub providers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteInspect {
    pub pattern: String,
    pub delivery: String,
    pub agents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyInspect {
    pub path: String,
    pub domains: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInspect {
    pub mode: String,
    pub status: String,
    pub generation_id: String,
    pub services: Vec<ServiceInspect>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInspect {
    pub name: String,
    pub state: String,
    pub detail: String,
}

/// Builds the inspect report from an already validated project config.
pub fn inspect_project(project: &ProjectConfig) -> Result<InspectReport, EvaError> {
    let runtime = RuntimeBuilder::new().build(project)?;
    Ok(InspectReport {
        project_root: display_path(&project.project_root),
        eva_config_path: display_path(&project.eva_config_path),
        environment: project.eva.runtime.env.clone(),
        hot_reload: project.eva.runtime.hot_reload,
        agents: project
            .agents
            .iter()
            .map(|agent| AgentInspect {
                id: agent.id.as_str().to_owned(),
                enabled: agent.enabled,
                script: display_path(&agent.script),
                subscriptions: agent
                    .subscriptions
                    .iter()
                    .map(|topic| topic.as_str().to_owned())
                    .collect(),
                parent: agent.parent.as_ref().map(|value| value.as_str().to_owned()),
                children: agent
                    .children
                    .iter()
                    .map(|value| value.as_str().to_owned())
                    .collect(),
            })
            .collect(),
        adapters: project
            .adapters
            .iter()
            .map(|adapter| AdapterInspect {
                id: adapter.id.as_str().to_owned(),
                name: adapter.name.clone(),
                enabled: adapter.enabled,
                transport: adapter.transport.as_str().to_owned(),
                capabilities: adapter
                    .capabilities
                    .iter()
                    .map(|capability| capability.as_str().to_owned())
                    .collect(),
            })
            .collect(),
        capabilities: project
            .capabilities
            .iter()
            .map(|capability| CapabilityInspect {
                id: capability.id.as_str().to_owned(),
                name: capability.name.clone(),
                enabled: capability.enabled,
                kind: capability.kind.as_str().to_owned(),
                capability: capability.capability.as_str().to_owned(),
                providers: capability
                    .adapter_providers()
                    .map(|provider| provider.as_str().to_owned())
                    .collect(),
            })
            .collect(),
        routes: project
            .routes
            .routes
            .iter()
            .map(|route| RouteInspect {
                pattern: route.pattern.as_str().to_owned(),
                delivery: route.delivery.as_str().to_owned(),
                agents: route
                    .agents
                    .iter()
                    .map(|agent| agent.as_str().to_owned())
                    .collect(),
            })
            .collect(),
        policies: project
            .policies
            .iter()
            .map(|policy| PolicyInspect {
                path: display_path(&policy.path),
                domains: policy.domains.keys().cloned().collect(),
            })
            .collect(),
        runtime: RuntimeInspect::from_summary(runtime.summary()),
    })
}

impl RuntimeInspect {
    fn from_summary(summary: &RuntimeSummary) -> Self {
        Self {
            mode: summary.mode.as_str().to_owned(),
            status: summary.status.as_str().to_owned(),
            generation_id: summary.generation_id.clone(),
            services: summary.services.iter().map(ServiceInspect::from).collect(),
        }
    }
}

impl From<&ServiceSummary> for ServiceInspect {
    fn from(summary: &ServiceSummary) -> Self {
        Self {
            name: summary.name.clone(),
            state: summary.state.as_str().to_owned(),
            detail: summary.detail.clone(),
        }
    }
}

impl InspectReport {
    pub fn to_json(&self) -> String {
        format!(
            "{{\"project_root\":{},\"eva_config_path\":{},\"environment\":{},\"hot_reload\":{},\"agents\":{},\"adapters\":{},\"capabilities\":{},\"routes\":{},\"policies\":{},\"runtime\":{}}}",
            json_string(&self.project_root),
            json_string(&self.eva_config_path),
            json_string(&self.environment),
            self.hot_reload,
            json_array(self.agents.iter().map(AgentInspect::to_json)),
            json_array(self.adapters.iter().map(AdapterInspect::to_json)),
            json_array(self.capabilities.iter().map(CapabilityInspect::to_json)),
            json_array(self.routes.iter().map(RouteInspect::to_json)),
            json_array(self.policies.iter().map(PolicyInspect::to_json)),
            self.runtime.to_json(),
        )
    }
}

impl AgentInspect {
    fn to_json(&self) -> String {
        format!(
            "{{\"id\":{},\"enabled\":{},\"script\":{},\"subscriptions\":{},\"parent\":{},\"children\":{}}}",
            json_string(&self.id),
            self.enabled,
            json_string(&self.script),
            json_array(self.subscriptions.iter().map(|value| json_string(value))),
            self.parent
                .as_ref()
                .map(|value| json_string(value))
                .unwrap_or_else(|| "null".to_owned()),
            json_array(self.children.iter().map(|value| json_string(value))),
        )
    }
}

impl AdapterInspect {
    fn to_json(&self) -> String {
        format!(
            "{{\"id\":{},\"name\":{},\"enabled\":{},\"transport\":{},\"capabilities\":{}}}",
            json_string(&self.id),
            json_string(&self.name),
            self.enabled,
            json_string(&self.transport),
            json_array(self.capabilities.iter().map(|value| json_string(value))),
        )
    }
}

impl CapabilityInspect {
    fn to_json(&self) -> String {
        format!(
            "{{\"id\":{},\"name\":{},\"enabled\":{},\"kind\":{},\"capability\":{},\"providers\":{}}}",
            json_string(&self.id),
            json_string(&self.name),
            self.enabled,
            json_string(&self.kind),
            json_string(&self.capability),
            json_array(self.providers.iter().map(|value| json_string(value))),
        )
    }
}

impl RouteInspect {
    fn to_json(&self) -> String {
        format!(
            "{{\"pattern\":{},\"delivery\":{},\"agents\":{}}}",
            json_string(&self.pattern),
            json_string(&self.delivery),
            json_array(self.agents.iter().map(|value| json_string(value))),
        )
    }
}

impl PolicyInspect {
    fn to_json(&self) -> String {
        format!(
            "{{\"path\":{},\"domains\":{}}}",
            json_string(&self.path),
            json_array(self.domains.iter().map(|value| json_string(value))),
        )
    }
}

impl RuntimeInspect {
    fn to_json(&self) -> String {
        format!(
            "{{\"mode\":{},\"status\":{},\"generation_id\":{},\"services\":{}}}",
            json_string(&self.mode),
            json_string(&self.status),
            json_string(&self.generation_id),
            json_array(self.services.iter().map(ServiceInspect::to_json)),
        )
    }
}

impl ServiceInspect {
    fn to_json(&self) -> String {
        format!(
            "{{\"name\":{},\"state\":{},\"detail\":{}}}",
            json_string(&self.name),
            json_string(&self.state),
            json_string(&self.detail),
        )
    }
}
