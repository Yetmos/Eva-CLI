//! V1.0 basic in-memory event loop with task diagnostics.

use crate::runtime::RuntimeSummary;
use crate::task::{
    DeadLetterSummary, ReplaySummary, RetryPolicy, TaskLogLevel, TaskReport, TaskStatus,
};
use eva_agent::{AgentHandlerOutput, AgentRunControl, AgentRunRecord, AgentRuntime};
use eva_capability::{CapabilityHostApi, CapabilityRouter};
use eva_config::{ProjectConfig, RouteDelivery};
use eva_core::{
    AgentId, EvaError, Event, EventId, EventPayload, GenerationId, InvokeInput, InvokeRequest,
    InvokeResponse, InvokeTarget, RequestId, Topic,
};
use eva_eventbus::{DeadLetterRecord, EventBus, EventReceipt, InMemoryEventBus};
use eva_lua_host::{
    LuaCancellationToken, LuaEventResult, LuaExecutionLimits, LuaGeneration, LuaHost,
    LuaHostContext, LuaHostObservation, LuaScript,
};
use eva_observability::{AuditAction, AuditSink, InMemoryAuditSink};
use eva_scheduler::{DeliveryMode, DeliveryPlan, MailboxRegistry, RoutingRule, SubscriptionTable};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

/// Runtime input for the built-in V1.0 basic example.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicRunOptions {
    pub event_id: EventId,
    pub request_id: RequestId,
    pub topic: Topic,
    pub payload: EventPayload,
    pub timeout_ms: Option<u64>,
    pub cancel_requested: bool,
    pub retry_attempts: usize,
    pub replay_dead_letters: bool,
}

/// Machine-readable report emitted by `eva run --example basic`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicRunReport {
    pub runtime_mode: String,
    pub generation_id: String,
    pub project_root: String,
    pub task: TaskReport,
    pub event_id: String,
    pub topic: String,
    pub receipt: EventReceipt,
    pub deliveries: Vec<DeliveryPlan>,
    pub agent_runs: Vec<AgentRunRecord>,
    pub lua_results: Vec<LuaEventResult>,
    pub lua_observability: Vec<LuaHostObservation>,
    pub lua_generation: LuaGeneration,
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
            timeout_ms: Some(30_000),
            cancel_requested: false,
            retry_attempts: 1,
            replay_dead_letters: false,
        }
    }
}

pub fn run_basic(
    summary: &RuntimeSummary,
    project: &ProjectConfig,
    options: BasicRunOptions,
) -> Result<BasicRunReport, EvaError> {
    let generation = GenerationId::parse(&summary.generation_id)?;
    let lua_generation = LuaGeneration::new(
        generation.clone(),
        project.agents.iter().filter(|agent| agent.enabled).count(),
    );
    let mut task = TaskReport::new(
        options.request_id.clone(),
        RetryPolicy::new(options.retry_attempts),
    );
    task.status = TaskStatus::Running;
    task.push_log(TaskLogLevel::Info, "task accepted by V1.0 core runtime");

    let event = Event::new(
        options.event_id.clone(),
        options.topic.clone(),
        options.payload.clone(),
    )
    .with_request_id(options.request_id.clone())
    .with_generation_id(generation);

    let mut audit = Vec::new();
    let mut event_bus = InMemoryEventBus::new();
    let receipt = event_bus.publish(event.clone())?;
    audit.push(format!("event.accepted:{}", receipt.event_id));
    task.push_log(
        TaskLogLevel::Info,
        format!("event accepted: {}", receipt.event_id),
    );

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
    task.push_log(
        TaskLogLevel::Info,
        format!("event delivered to {} mailbox(es)", deliveries.len()),
    );

    let mut agents = agent_runtimes(project)?;
    let lua_host = LuaHost::new();
    let mut agent_runs = Vec::new();
    let mut lua_results = Vec::new();
    let mut lua_observability = Vec::new();
    let mut lua_audit_sink = InMemoryAuditSink::default();
    let lua_tool_host: Rc<dyn CapabilityHostApi> = Rc::new(CapabilityRouter::with_v04_builtins());

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
        let control = agent_control(&options);
        let lua_limits = lua_execution_limits(&options);
        let record = agent
            .run_next_with_control(control, |agent_id, event| {
                let result = lua_host.run_on_event_with_tools_and_limits(
                    &script,
                    event,
                    &LuaHostContext::new(agent_id.clone()),
                    Rc::clone(&lua_tool_host),
                    lua_limits.clone(),
                )?;
                record_lua_observability(
                    &mut lua_audit_sink,
                    &mut task,
                    &mut audit,
                    &result.observability,
                )?;
                lua_observability.extend(result.observability.clone());
                lua_results.push(result.clone());
                Ok(AgentHandlerOutput::new(result.status, result.note))
            })
            .ok_or_else(|| EvaError::internal("agent queue was empty after delivery"))?;

        if let Some(error) = record.error.clone() {
            event_bus.dead_letter(event.clone(), error.clone());
            event_bus.fail(event.event_id(), delivery.agent_id.clone(), error)?;
            let failure = record
                .error
                .clone()
                .expect("error branch has structured failure");
            if record.status == eva_agent::AgentRunStatus::Cancelled {
                task.cancel(failure.message());
                task.push_log(
                    TaskLogLevel::Warning,
                    format!(
                        "agent {} run cancelled: {}",
                        delivery.agent_id,
                        failure.message()
                    ),
                );
            } else {
                task.fail(record.attempts, failure.clone());
                task.push_log(
                    TaskLogLevel::Error,
                    format!(
                        "agent {} ended as {}: {}",
                        delivery.agent_id,
                        record.status.as_str(),
                        failure.message()
                    ),
                );
            }
        } else {
            event_bus.ack(event.event_id(), delivery.agent_id.clone())?;
            task.complete(record.attempts);
            task.push_log(
                TaskLogLevel::Info,
                format!(
                    "agent {} handled event in {} attempt(s)",
                    delivery.agent_id, record.attempts
                ),
            );
        }
        agent_runs.push(record);
    }

    let capability_response = invoke_first_lua_capability(&lua_results, &options.request_id)?;
    if capability_response.is_some() {
        audit.push("capability.invoked:1".to_owned());
        task.push_log(TaskLogLevel::Info, "capability invoked: config.lint");
    }

    let replay_receipts = if options.replay_dead_letters {
        event_bus.replay_dead_letters()?
    } else {
        Vec::new()
    };
    if !event_bus.dead_letters().is_empty() {
        audit.push(format!(
            "dead_letter.recorded:{}",
            event_bus.dead_letters().len()
        ));
        task.push_log(
            TaskLogLevel::Warning,
            format!("dead letters recorded: {}", event_bus.dead_letters().len()),
        );
    }
    if !replay_receipts.is_empty() {
        audit.push(format!("dead_letter.replayed:{}", replay_receipts.len()));
        task.push_log(
            TaskLogLevel::Info,
            format!("dead letters replayed: {}", replay_receipts.len()),
        );
    }
    task.dead_letters = dead_letter_summaries(event_bus.dead_letters());
    task.replayed_events = replay_summaries(&replay_receipts);

    Ok(BasicRunReport {
        runtime_mode: summary.mode.to_string(),
        generation_id: summary.generation_id.clone(),
        project_root: project.project_root.display().to_string(),
        task,
        event_id: event.event_id().to_string(),
        topic: event.topic().to_string(),
        receipt,
        deliveries,
        agent_runs,
        lua_results,
        lua_observability,
        lua_generation,
        capability_response,
        audit,
    })
}

fn record_lua_observability(
    audit_sink: &mut impl AuditSink,
    task: &mut TaskReport,
    audit: &mut Vec<String>,
    observations: &[LuaHostObservation],
) -> Result<(), EvaError> {
    for observation in observations {
        audit_sink.record(observation.to_audit_event())?;
        let message = observation.message.as_deref().unwrap_or_default();
        audit.push(format!("{}:{}", observation.action.as_str(), message));
        match observation.action {
            AuditAction::LuaHostLog => {
                task.push_log(log_level(observation), format!("lua host log: {message}"))
            }
            AuditAction::LuaHostAudit => {
                task.push_log(TaskLogLevel::Info, format!("lua host audit: {message}"));
            }
            _ => {}
        }
    }
    Ok(())
}

fn log_level(observation: &LuaHostObservation) -> TaskLogLevel {
    match observation
        .fields
        .iter()
        .find(|(key, _)| key == "level")
        .map(|(_, value)| value.as_str())
    {
        Some("warn" | "warning") => TaskLogLevel::Warning,
        Some("error") => TaskLogLevel::Error,
        _ => TaskLogLevel::Info,
    }
}

fn agent_control(options: &BasicRunOptions) -> AgentRunControl {
    let mut control = AgentRunControl::default()
        .with_max_attempts(options.retry_attempts)
        .with_cancel_requested(options.cancel_requested);
    if let Some(timeout_ms) = options.timeout_ms {
        control = control.with_timeout(Duration::from_millis(timeout_ms));
    }
    control
}

fn lua_execution_limits(options: &BasicRunOptions) -> LuaExecutionLimits {
    let mut limits = options
        .timeout_ms
        .map(|timeout_ms| LuaExecutionLimits::with_timeout(Duration::from_millis(timeout_ms)))
        .unwrap_or_default();
    if options.cancel_requested {
        let token = LuaCancellationToken::new();
        token.cancel();
        limits = limits.with_cancellation_token(token);
    }
    limits
}

fn dead_letter_summaries(records: &[DeadLetterRecord]) -> Vec<DeadLetterSummary> {
    records
        .iter()
        .map(|record| DeadLetterSummary {
            event_id: record.event.event_id().to_string(),
            topic: record.event.topic().to_string(),
            reason_kind: record.reason.kind().as_str().to_owned(),
            reason: record.reason.message().to_owned(),
            replay_count: record.replay_count,
        })
        .collect()
}

fn replay_summaries(receipts: &[EventReceipt]) -> Vec<ReplaySummary> {
    receipts
        .iter()
        .map(|receipt| ReplaySummary {
            event_id: receipt.event_id.to_string(),
            sequence: receipt.sequence,
            topic: receipt.topic.to_string(),
        })
        .collect()
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
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    fn missing_route_returns_structured_error() {
        let project = load_project_config(workspace_root()).unwrap();
        let runtime = RuntimeBuilder::in_memory_v05().build(&project).unwrap();
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
        let runtime = RuntimeBuilder::in_memory_v05().build(&project).unwrap();

        let report = runtime
            .run_basic(&project, BasicRunOptions::default())
            .unwrap();

        assert_eq!(report.deliveries[0].agent_id.as_str(), "root-agent");
        assert_eq!(
            report.agent_runs[0].handler_status.as_deref(),
            Some("accepted")
        );
        assert!(report.lua_results[0].capability.is_none());
        assert!(report.lua_results[0]
            .note
            .as_deref()
            .is_some_and(|note| note.contains("tool=completed") && note.contains("valid")));
        assert_eq!(report.task.status, TaskStatus::Completed);
        assert_eq!(report.task.attempts, 1);
        assert_eq!(report.lua_generation.script_count, 1);
        assert_eq!(report.lua_observability.len(), 2);
        assert!(report.lua_observability.iter().any(|event| event.action
            == AuditAction::LuaHostLog
            && event
                .message
                .as_deref()
                .is_some_and(|message| message.contains("root-agent accepted"))));
        assert!(report
            .lua_observability
            .iter()
            .any(|event| event.action == AuditAction::LuaHostAudit
                && event.trace.request_id.as_ref().is_some()));
        assert!(report
            .audit
            .iter()
            .any(|entry| entry.starts_with("lua.host.audit:root-agent requested")));
        assert!(report
            .task
            .logs
            .iter()
            .any(|entry| entry.message.contains("lua host log")));
        assert!(report.capability_response.is_none());
    }

    #[test]
    fn lua_vm_timeout_limit_interrupts_non_returning_script() {
        let root = test_root("lua-timeout-limit");
        copy_dir_all(workspace_root().join("examples/basic"), root.path()).unwrap();
        copy_dir_all(
            workspace_root().join("config/schemas"),
            root.path().join("config/schemas"),
        )
        .unwrap();
        fs::write(
            root.path().join("config/eva.yaml"),
            fs::read_to_string(root.path().join("config/eva.yaml"))
                .unwrap()
                .replace(
                    "schema_dir: ../../config/schemas",
                    "schema_dir: config/schemas",
                ),
        )
        .unwrap();
        fs::write(
            root.path().join("config/agents/root-agent/main.lua"),
            r#"
local root = {}

function root.on_event(event, ctx)
  while true do
  end
  return { status = "unreachable" }
end

return root
"#,
        )
        .unwrap();
        let project = load_project_config(root.path()).unwrap();
        let runtime = RuntimeBuilder::in_memory_v05().build(&project).unwrap();

        let report = runtime
            .run_basic(
                &project,
                BasicRunOptions {
                    timeout_ms: Some(1),
                    ..BasicRunOptions::default()
                },
            )
            .unwrap();

        assert_eq!(report.task.status, TaskStatus::TimedOut);
        assert_eq!(
            report.agent_runs[0].status,
            eva_agent::AgentRunStatus::TimedOut
        );
        assert_eq!(
            report.agent_runs[0]
                .error
                .as_ref()
                .unwrap()
                .provider_code()
                .unwrap()
                .as_str(),
            "lua_timeout"
        );
        assert!(report.lua_results.is_empty());
        assert!(report.capability_response.is_none());
    }

    #[test]
    fn cancelled_basic_run_returns_task_record() {
        let project = load_project_config(workspace_root().join("examples/basic")).unwrap();
        let runtime = RuntimeBuilder::in_memory_v05().build(&project).unwrap();

        let report = runtime
            .run_basic(
                &project,
                BasicRunOptions {
                    cancel_requested: true,
                    ..BasicRunOptions::default()
                },
            )
            .unwrap();

        assert_eq!(report.task.status, TaskStatus::Cancelled);
        assert!(report.task.cancellation.requested);
        assert_eq!(
            report.agent_runs[0].status,
            eva_agent::AgentRunStatus::Cancelled
        );
        assert_eq!(report.task.dead_letters.len(), 1);
    }

    #[test]
    fn timeout_basic_run_records_dead_letter_and_replay() {
        let project = load_project_config(workspace_root().join("examples/basic")).unwrap();
        let runtime = RuntimeBuilder::in_memory_v05().build(&project).unwrap();

        let report = runtime
            .run_basic(
                &project,
                BasicRunOptions {
                    timeout_ms: Some(0),
                    replay_dead_letters: true,
                    ..BasicRunOptions::default()
                },
            )
            .unwrap();

        assert_eq!(report.task.status, TaskStatus::TimedOut);
        assert_eq!(report.task.dead_letters[0].event_id, "evt-basic-1");
        assert_eq!(report.task.dead_letters[0].replay_count, 1);
        assert_eq!(
            report.task.replayed_events[0].event_id,
            "evt-basic-1:replay-1"
        );
    }

    fn copy_dir_all(source: impl AsRef<Path>, target: impl AsRef<Path>) -> std::io::Result<()> {
        fs::create_dir_all(target.as_ref())?;
        for entry in fs::read_dir(source.as_ref())? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let target_path = target.as_ref().join(entry.file_name());
            if file_type.is_dir() {
                copy_dir_all(entry.path(), target_path)?;
            } else {
                fs::copy(entry.path(), target_path)?;
            }
        }
        Ok(())
    }

    fn test_root(name: &str) -> TestRoot {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "eva-runtime-basic-{name}-{}-{now}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        TestRoot { path }
    }

    struct TestRoot {
        path: PathBuf,
    }

    impl TestRoot {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
