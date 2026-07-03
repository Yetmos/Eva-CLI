//! V0.4 basic in-memory event loop.

use crate::runtime::RuntimeSummary;
use eva_agent::{AgentHandlerOutput, AgentRunRecord, AgentRuntime};
use eva_capability::{CapabilityHostApi, CapabilityRouter};
use eva_config::{ProjectConfig, RouteDelivery};
use eva_core::{
    AgentId, EvaError, Event, EventId, EventPayload, GenerationId, InvokeInput, InvokeRequest,
    InvokeResponse, InvokeTarget, RequestId, Topic,
};
use eva_eventbus::{EventBus, EventReceipt, InMemoryEventBus};
use eva_lua_host::{LuaEventResult, LuaHost, LuaHostContext, LuaScript};
use eva_scheduler::{DeliveryMode, DeliveryPlan, MailboxRegistry, RoutingRule, SubscriptionTable};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Runtime input for the built-in V0.4 basic example.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicRunOptions {
    pub event_id: EventId,
    pub request_id: RequestId,
    pub topic: Topic,
    pub payload: EventPayload,
}

/// Machine-readable report emitted by `eva run --example basic`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicRunReport {
    pub runtime_mode: String,
    pub generation_id: String,
    pub project_root: String,
    pub event_id: String,
    pub topic: String,
    pub receipt: EventReceipt,
    pub deliveries: Vec<DeliveryPlan>,
    pub agent_runs: Vec<AgentRunRecord>,
    pub lua_results: Vec<LuaEventResult>,
    pub capability_response: Option<InvokeResponse>,
    pub audit: Vec<String>,
}

impl Default for BasicRunOptions {
    fn default() -> Self {
        Self {
            event_id: EventId::parse("evt-basic-1").expect("static event id is valid"),
            request_id: RequestId::parse("req-basic-1").expect("static request id is valid"),
            topic: Topic::parse("/input/user").expect("static topic is valid"),
            payload: EventPayload::text("basic example"),
        }
    }
}

pub fn run_basic(
    summary: &RuntimeSummary,
    project: &ProjectConfig,
    options: BasicRunOptions,
) -> Result<BasicRunReport, EvaError> {
    let generation = GenerationId::parse(&summary.generation_id)?;
    let event = Event::new(
        options.event_id.clone(),
        options.topic.clone(),
        options.payload,
    )
    .with_request_id(options.request_id.clone())
    .with_generation_id(generation);

    let mut audit = Vec::new();
    let mut event_bus = InMemoryEventBus::new();
    let receipt = event_bus.publish(event.clone())?;
    audit.push(format!("event.accepted:{}", receipt.event_id));

    let table = subscription_table(project);
    let mut mailboxes = register_mailboxes(project)?;
    let deliveries = match table.deliver(&mut mailboxes, &event) {
        Ok(deliveries) => deliveries,
        Err(error) => {
            event_bus.dead_letter(event.clone(), error.clone());
            return Err(error);
        }
    };
    audit.push(format!("event.delivered:{}", deliveries.len()));

    let mut agents = agent_runtimes(project)?;
    let lua_host = LuaHost::new();
    let mut agent_runs = Vec::new();
    let mut lua_results = Vec::new();

    for delivery in &deliveries {
        let delivered_event = mailboxes
            .drain_one(&delivery.agent_id)?
            .ok_or_else(|| EvaError::internal("scheduler delivery did not populate mailbox"))?;
        let agent = agents.get_mut(&delivery.agent_id).ok_or_else(|| {
            EvaError::not_found("agent runtime is not registered")
                .with_context("agent_id", delivery.agent_id.as_str())
        })?;
        agent.accept(delivered_event)?;
        let script = load_agent_script(project, &delivery.agent_id)?;
        let record = agent
            .run_next(|agent_id, event| {
                let result = lua_host.run_on_event(
                    &script,
                    event,
                    &LuaHostContext {
                        agent_id: agent_id.clone(),
                    },
                )?;
                lua_results.push(result.clone());
                Ok(AgentHandlerOutput::new(result.status, result.note))
            })
            .ok_or_else(|| EvaError::internal("agent queue was empty after delivery"))?;

        if let Some(error) = record.error.clone() {
            event_bus.fail(event.event_id(), delivery.agent_id.clone(), error)?;
        } else {
            event_bus.ack(event.event_id(), delivery.agent_id.clone())?;
        }
        agent_runs.push(record);
    }

    let capability_response = invoke_first_lua_capability(&lua_results, &options.request_id)?;
    if capability_response.is_some() {
        audit.push("capability.invoked:1".to_owned());
    }

    Ok(BasicRunReport {
        runtime_mode: summary.mode.to_string(),
        generation_id: summary.generation_id.clone(),
        project_root: project.project_root.display().to_string(),
        event_id: event.event_id().to_string(),
        topic: event.topic().to_string(),
        receipt,
        deliveries,
        agent_runs,
        lua_results,
        capability_response,
        audit,
    })
}

fn subscription_table(project: &ProjectConfig) -> SubscriptionTable {
    let rules = project
        .routes
        .routes
        .iter()
        .map(|route| {
            RoutingRule::new(
                route.pattern.clone(),
                match route.delivery {
                    RouteDelivery::Fanout => DeliveryMode::Fanout,
                    RouteDelivery::Compete => DeliveryMode::Compete,
                },
                route.agents.clone(),
            )
        })
        .collect();
    SubscriptionTable::new(rules)
}

fn register_mailboxes(project: &ProjectConfig) -> Result<MailboxRegistry, EvaError> {
    let mut registry = MailboxRegistry::new();
    for agent in project.agents.iter().filter(|agent| agent.enabled) {
        registry.register(agent.id.clone(), 256)?;
    }
    Ok(registry)
}

fn agent_runtimes(project: &ProjectConfig) -> Result<BTreeMap<AgentId, AgentRuntime>, EvaError> {
    let mut runtimes = BTreeMap::new();
    for agent in project.agents.iter().filter(|agent| agent.enabled) {
        let mut runtime = AgentRuntime::new(agent.id.clone(), 256)?;
        runtime.start()?;
        runtimes.insert(agent.id.clone(), runtime);
    }
    Ok(runtimes)
}

fn load_agent_script(project: &ProjectConfig, agent_id: &AgentId) -> Result<LuaScript, EvaError> {
    let manifest = project
        .agents
        .iter()
        .find(|agent| &agent.id == agent_id)
        .ok_or_else(|| {
            EvaError::not_found("agent manifest is missing")
                .with_context("agent_id", agent_id.as_str())
        })?;
    LuaScript::load(resolve_script_path(&manifest.path, &manifest.script))
}

fn resolve_script_path(manifest_path: &Path, script: &Path) -> PathBuf {
    if script.is_absolute() {
        script.to_path_buf()
    } else {
        manifest_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(script)
    }
}

fn invoke_first_lua_capability(
    lua_results: &[LuaEventResult],
    request_id: &RequestId,
) -> Result<Option<InvokeResponse>, EvaError> {
    let Some(result) = lua_results
        .iter()
        .find(|result| result.capability.is_some())
    else {
        return Ok(None);
    };
    let capability = result
        .capability
        .clone()
        .expect("checked capability presence");
    let request = InvokeRequest::new(
        RequestId::parse(&format!("{}:capability", request_id.as_str()))?,
        InvokeTarget::Capability(capability),
        InvokeInput::text(result.capability_input.clone().unwrap_or_default()),
    );
    Ok(Some(CapabilityRouter::with_v04_builtins().invoke(request)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeBuilder;
    use eva_config::load_project_config;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    fn missing_route_returns_structured_error() {
        let project = load_project_config(workspace_root()).unwrap();
        let runtime = RuntimeBuilder::in_memory_v04().build(&project).unwrap();
        let options = BasicRunOptions {
            topic: Topic::parse("/missing/topic").unwrap(),
            ..BasicRunOptions::default()
        };

        let error = runtime.run_basic(&project, options).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::NotFound);
    }

    #[test]
    fn basic_example_runs_event_to_lua_and_capability() {
        let project = load_project_config(workspace_root().join("examples/basic")).unwrap();
        let runtime = RuntimeBuilder::in_memory_v04().build(&project).unwrap();

        let report = runtime
            .run_basic(&project, BasicRunOptions::default())
            .unwrap();

        assert_eq!(report.deliveries[0].agent_id.as_str(), "root-agent");
        assert_eq!(
            report.agent_runs[0].handler_status.as_deref(),
            Some("accepted")
        );
        assert_eq!(
            report.lua_results[0].capability.as_ref().unwrap().as_str(),
            "config.lint"
        );
        assert!(report.capability_response.unwrap().is_success());
    }
}
