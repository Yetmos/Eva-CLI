//! 提供基础模式下单次事件从发布、路由、邮箱投递到代理执行的内存事件循环。
//!
//! 循环按“发布事件、生成投递计划、逐邮箱取件、运行 Lua、确认或死信、可选重放”的顺序
//! 推进，并同步更新任务诊断。该模式没有持久化幂等保证；死信重放仅作用于本次运行所持有的
//! 内存事件总线，适合示例和边界验证而非崩溃恢复。
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

/// 定义基础事件循环的输入、取消、超时和死信重放选项。
/// Runtime input for the built-in V1.0 basic example.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicRunOptions {
    /// 记录 `event_id` 字段对应的值。
    pub event_id: EventId,
    /// 记录 `request_id` 字段对应的值。
    pub request_id: RequestId,
    /// 记录 `topic` 字段对应的值。
    pub topic: Topic,
    /// 记录 `payload` 字段对应的值。
    pub payload: EventPayload,
    /// 同时约束代理运行与 Lua VM；`None` 表示不设置时间截止点。
    pub timeout_ms: Option<u64>,
    /// 在代理与 Lua 执行前预置取消信号，使任务不进入后续处理。
    pub cancel_requested: bool,
    /// 允许代理处理器尝试的最大次数，并写入最终任务报告。
    pub retry_attempts: usize,
    /// 控制是否在本次循环末尾重放刚产生的内存死信。
    pub replay_dead_letters: bool,
}

/// 汇总事件循环的投递、代理结果、Lua 结果、任务终态和审计证据。
/// Machine-readable report emitted by `eva run --example basic`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicRunReport {
    /// 记录 `runtime_mode` 字段对应的值。
    pub runtime_mode: String,
    /// 记录 `generation_id` 字段对应的值。
    pub generation_id: String,
    /// 记录 `project_root` 字段对应的值。
    pub project_root: String,
    /// 记录 `task` 字段对应的值。
    pub task: TaskReport,
    /// 记录 `event_id` 字段对应的值。
    pub event_id: String,
    /// 记录 `topic` 字段对应的值。
    pub topic: String,
    /// 记录 `receipt` 字段对应的值。
    pub receipt: EventReceipt,
    /// 记录 `deliveries` 字段对应的值。
    pub deliveries: Vec<DeliveryPlan>,
    /// 记录 `agent_runs` 字段对应的值。
    pub agent_runs: Vec<AgentRunRecord>,
    /// 记录 `lua_results` 字段对应的值。
    pub lua_results: Vec<LuaEventResult>,
    /// 记录 `lua_observability` 字段对应的值。
    pub lua_observability: Vec<LuaHostObservation>,
    /// 记录 `lua_generation` 字段对应的值。
    pub lua_generation: LuaGeneration,
    /// 记录 `capability_response` 字段对应的值。
    pub capability_response: Option<InvokeResponse>,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

/// 为相关类型实现其约定的行为与方法。
impl Default for BasicRunOptions {
    /// 创建采用该类型默认配置的实例。
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

/// 执行一次完整的基础事件循环。
///
/// 路由失败会先把原事件写入死信再返回错误；每个代理成功后才确认其投递，失败或取消则先
/// 记录死信与失败确认再推进任务状态。能力调用发生在所有代理运行结束之后，并且只采用第一
/// 个声明了能力的 Lua 结果，避免同一事件隐式触发多个能力副作用。
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
    // 发布成功的 receipt 是后续投递、确认与报告共同引用的事件身份。
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
            // 路由失败时事件尚未进入任何代理邮箱，死信是唯一保留的失败证据。
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

    // 投递计划顺序决定代理执行顺序；基础模式有意串行运行，便于诊断和确定性测试。
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
            // 失败投递绝不能 ack；先保存死信，再写入代理维度的失败状态。
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
            // 只有处理器无结构化错误时才确认，保证 ack 表示业务处理确实完成。
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

    // 重放在所有原始投递完成后集中执行，不与当前代理循环交错。
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

/// 先写入审计接收器，再镜像到任务日志；审计失败会中止当前代理处理而不会伪造成功日志。
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

/// 执行 `log_level` 对应的处理逻辑。
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

/// 从同一组选项构造代理重试、取消与超时控制，保持任务与 VM 限额一致。
fn agent_control(options: &BasicRunOptions) -> AgentRunControl {
    let mut control = AgentRunControl::default()
        .with_max_attempts(options.retry_attempts)
        .with_cancel_requested(options.cancel_requested);
    if let Some(timeout_ms) = options.timeout_ms {
        control = control.with_timeout(Duration::from_millis(timeout_ms));
    }
    control
}

/// 构造 Lua 时间和取消限制；预取消令牌确保脚本在首次钩子检查时终止。
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

/// 执行 `dead_letter_summaries` 对应的处理逻辑。
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

/// 执行 `replay_summaries` 对应的恢复或重驱流程。
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

/// 执行 `subscription_table` 对应的处理逻辑。
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

/// 只为启用的代理注册固定容量邮箱，避免路由到未启动代理。
fn register_mailboxes(project: &ProjectConfig) -> Result<MailboxRegistry, EvaError> {
    let mut registry = MailboxRegistry::new();
    for agent in project.agents.iter().filter(|agent| agent.enabled) {
        registry.register(agent.id.clone(), 256)?;
    }
    Ok(registry)
}

/// 为每个启用代理创建并启动独立运行时；任一启动失败都会中止整个基础循环。
fn agent_runtimes(project: &ProjectConfig) -> Result<BTreeMap<AgentId, AgentRuntime>, EvaError> {
    let mut runtimes = BTreeMap::new();
    for agent in project.agents.iter().filter(|agent| agent.enabled) {
        let mut runtime = AgentRuntime::new(agent.id.clone(), 256)?;
        runtime.start()?;
        runtimes.insert(agent.id.clone(), runtime);
    }
    Ok(runtimes)
}

/// 读取 `load_agent_script` 所需的持久化数据，失败时保留错误上下文。
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

/// 执行 `resolve_script_path` 对应的处理逻辑。
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

/// 调用首个 Lua 结果声明的能力，并派生独立请求标识；没有声明时无副作用地返回 `None`。
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

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeBuilder;
    use eva_config::load_project_config;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 执行 `workspace_root` 对应的处理逻辑。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    /// 验证 `missing_route_returns_structured_error` 场景下的预期行为。
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

    /// 验证 `basic_example_runs_event_to_lua_and_capability` 场景下的预期行为。
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

    /// 验证 `lua_vm_timeout_limit_interrupts_non_returning_script` 场景下的预期行为。
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

    /// 验证 `cancelled_basic_run_returns_task_record` 场景下的预期行为。
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

    /// 验证 `timeout_basic_run_records_dead_letter_and_replay` 场景下的预期行为。
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

    /// 执行 `copy_dir_all` 对应的处理逻辑。
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

    /// 执行 `test_root` 对应的处理逻辑。
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

    /// 表示 `TestRoot` 数据结构。
    struct TestRoot {
        /// 记录 `path` 字段对应的值。
        path: PathBuf,
    }

    /// 为相关类型实现其约定的行为与方法。
    impl TestRoot {
        /// 返回 `path` 对应的数据视图。
        fn path(&self) -> &Path {
            &self.path
        }
    }

    /// 为相关类型实现其约定的行为与方法。
    impl Drop for TestRoot {
        /// 停止、取消或释放 `drop` 管理的状态。
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
