//! 构建 Lua 可见的事件、上下文、工具调用与可观测性绑定。
//!
//! 工具调用只能通过注入的 `CapabilityHostApi`，每次调用生成独立请求标识并服从取消与资源
//! 限额。兼容静态解析器只在特定旧格式且 VM 无法处理时回退，不会吞掉权限或运行时错误。
//! Typed host bindings exposed to the controlled Lua contract.

use crate::loader::LuaScript;
use crate::sandbox::LuaSandboxPolicy;
use crate::vm::{LuaExecutionLimits, LuaVmAdapter, MluaVmAdapter};
use eva_capability::CapabilityHostApi;
use eva_core::{AgentId, CapabilityName, EvaError, Event, Topic};
use eva_memory::LuaContextSnapshot;
use eva_observability::{AuditAction, AuditEvent, AuditOutcome, TraceFields};
use std::rc::Rc;

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "typed host API bindings exposed to Lua";

/// 表示 `LuaHostContext` 数据结构。
/// Context passed to a Lua `on_event` handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaHostContext {
    /// 记录 `agent_id` 字段对应的值。
    pub agent_id: AgentId,
    /// 记录 `context` 字段对应的值。
    pub context: LuaContextSnapshot,
}

/// 表示 `LuaHostObservation` 数据结构。
/// Host observation emitted by `ctx.host.log` and `ctx.host.audit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaHostObservation {
    /// 记录 `action` 字段对应的值。
    pub action: AuditAction,
    /// 记录 `outcome` 字段对应的值。
    pub outcome: AuditOutcome,
    /// 记录 `trace` 字段对应的值。
    pub trace: TraceFields,
    /// 记录 `message` 字段对应的值。
    pub message: Option<String>,
    /// 记录 `fields` 字段对应的值。
    pub fields: Vec<(String, String)>,
}

/// 表示 `LuaEventResult` 数据结构。
/// Controlled result returned by the V0.4 Lua host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaEventResult {
    /// 记录 `agent_id` 字段对应的值。
    pub agent_id: AgentId,
    /// 记录 `status` 字段对应的值。
    pub status: String,
    /// 记录 `topic` 字段对应的值。
    pub topic: Topic,
    /// 记录 `note` 字段对应的值。
    pub note: Option<String>,
    /// 记录 `capability` 字段对应的值。
    pub capability: Option<CapabilityName>,
    /// 记录 `capability_input` 字段对应的值。
    pub capability_input: Option<String>,
    /// 记录 `context` 字段对应的值。
    pub context: LuaContextSnapshot,
    /// 记录 `observability` 字段对应的值。
    pub observability: Vec<LuaHostObservation>,
}

/// 表示 `LuaHost` 数据结构。
/// Synchronous controlled Lua host facade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaHost<A = MluaVmAdapter> {
    /// 记录 `sandbox` 字段对应的值。
    sandbox: LuaSandboxPolicy,
    /// 记录 `vm` 字段对应的值。
    vm: A,
}

impl LuaHost<MluaVmAdapter> {
    /// 创建并初始化当前类型的实例。
    pub fn new() -> Self {
        Self::with_vm_adapter(MluaVmAdapter)
    }
}

impl<A> LuaHost<A> {
    /// 设置 `vm_adapter` 并返回更新后的实例。
    pub fn with_vm_adapter(vm: A) -> Self {
        Self {
            sandbox: LuaSandboxPolicy::default(),
            vm,
        }
    }
}

impl<A: LuaVmAdapter> LuaHost<A> {
    /// 执行 `run_on_event` 对应的受控流程。
    pub fn run_on_event(
        &self,
        script: &LuaScript,
        event: &Event,
        ctx: &LuaHostContext,
    ) -> Result<LuaEventResult, EvaError> {
        self.run_on_event_with_limits(script, event, ctx, LuaExecutionLimits::default())
    }

    /// 执行 `run_on_event_with_limits` 对应的受控流程。
    pub fn run_on_event_with_limits(
        &self,
        script: &LuaScript,
        event: &Event,
        ctx: &LuaHostContext,
        limits: LuaExecutionLimits,
    ) -> Result<LuaEventResult, EvaError> {
        self.sandbox.validate(script)?;
        match self.vm.run_on_event_with_limits(script, event, ctx, limits) {
            Ok(result) => Ok(result),
            Err(error) if should_attempt_static_fallback(script.source(), &error) => {
                parse_static_on_event(script, event, ctx)
            }
            Err(error) => Err(error),
        }
    }

    /// 执行 `run_on_event_with_tools` 对应的受控流程。
    pub fn run_on_event_with_tools(
        &self,
        script: &LuaScript,
        event: &Event,
        ctx: &LuaHostContext,
        tool_host: Rc<dyn CapabilityHostApi>,
    ) -> Result<LuaEventResult, EvaError> {
        self.run_on_event_with_tools_and_limits(
            script,
            event,
            ctx,
            tool_host,
            LuaExecutionLimits::default(),
        )
    }

    /// 执行 `run_on_event_with_tools_and_limits` 对应的受控流程。
    pub fn run_on_event_with_tools_and_limits(
        &self,
        script: &LuaScript,
        event: &Event,
        ctx: &LuaHostContext,
        tool_host: Rc<dyn CapabilityHostApi>,
        limits: LuaExecutionLimits,
    ) -> Result<LuaEventResult, EvaError> {
        self.sandbox.validate(script)?;
        match self
            .vm
            .run_on_event_with_tools_and_limits(script, event, ctx, tool_host, limits)
        {
            Ok(result) => Ok(result),
            Err(error) if should_attempt_static_fallback(script.source(), &error) => {
                parse_static_on_event(script, event, ctx)
            }
            Err(error) => Err(error),
        }
    }
}

/// 读取或解析 `parse_static_on_event` 所需的数据，失败时保留错误语义。
fn parse_static_on_event(
    script: &LuaScript,
    event: &Event,
    ctx: &LuaHostContext,
) -> Result<LuaEventResult, EvaError> {
    let source = script.source();
    if !source.contains("on_event") {
        return Err(EvaError::invalid_argument(
            "Lua script does not define on_event",
        ));
    }

    let agent_id = extract_string(source, "agent_id")
        .map(|value| AgentId::parse(&value))
        .transpose()?
        .unwrap_or_else(|| ctx.agent_id.clone());
    let status = extract_string(source, "status").unwrap_or_else(|| "handled".to_owned());
    let topic = extract_string(source, "topic")
        .map(|value| Topic::parse(&value))
        .transpose()?
        .unwrap_or_else(|| event.topic().clone());
    let note = extract_string(source, "note");
    let capability = extract_string(source, "capability")
        .map(|value| CapabilityName::parse(&value))
        .transpose()?;
    let capability_input = extract_string(source, "capability_input")
        .or_else(|| event.payload().as_text().map(str::to_owned));

    Ok(LuaEventResult {
        agent_id,
        status,
        topic,
        note,
        capability,
        capability_input,
        context: ctx.context.clone(),
        observability: Vec::new(),
    })
}

/// 执行 `should_attempt_static_fallback` 对应的处理逻辑。
fn should_attempt_static_fallback(source: &str, error: &EvaError) -> bool {
    let is_load_failure = error
        .provider_code()
        .map(|code| code.as_str() == "lua_syntax_error" || code.as_str() == "lua_load_error")
        .unwrap_or(false);
    is_load_failure
        && source.contains("on_event")
        && !source.contains("function")
        && (source.contains("status =")
            || source.contains("capability =")
            || source.contains("note ="))
}

impl LuaHostContext {
    /// 创建并初始化当前类型的实例。
    pub fn new(agent_id: AgentId) -> Self {
        Self {
            agent_id,
            context: LuaContextSnapshot::default(),
        }
    }

    /// 设置 `context` 并返回更新后的实例。
    pub fn with_context(mut self, context: LuaContextSnapshot) -> Self {
        self.context = context;
        self
    }
}

impl LuaHostObservation {
    /// 创建并初始化当前类型的实例。
    pub fn new(action: AuditAction, trace: TraceFields) -> Self {
        Self {
            action,
            outcome: AuditOutcome::Ok,
            trace,
            message: None,
            fields: Vec::new(),
        }
    }

    /// 设置 `message` 并返回更新后的实例。
    pub fn with_message(mut self, value: impl Into<String>) -> Self {
        self.message = Some(value.into());
        self
    }

    /// 设置 `field` 并返回更新后的实例。
    pub fn with_field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.push((key.into(), value.into()));
        self
    }

    /// 将当前值按 `to_audit_event` 约定的形式转换。
    pub fn to_audit_event(&self) -> AuditEvent {
        let mut event = AuditEvent::new(self.action, self.outcome, self.trace.clone());
        if let Some(message) = &self.message {
            event = event.with_message(message.clone());
        }
        for (key, value) in &self.fields {
            event = event.with_field(key.clone(), value.clone());
        }
        event
    }
}

impl Default for LuaHost<MluaVmAdapter> {
    /// 创建使用默认受控 VM 适配器的 Lua 宿主。
    fn default() -> Self {
        Self::new()
    }
}

/// 读取或解析 `extract_string` 所需的数据，失败时保留错误语义。
fn extract_string(source: &str, key: &str) -> Option<String> {
    let marker = format!("{key} =");
    let line = source.lines().find(|line| line.contains(&marker))?;
    let start = line.find(&marker)? + marker.len();
    let rest = line[start..].trim_start();
    let quote_start = rest.find('"')? + 1;
    let after_quote = &rest[quote_start..];
    let quote_end = after_quote.find('"')?;
    Some(after_quote[..quote_end].to_owned())
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::LuaCancellationToken;
    use eva_capability::{
        CapabilityDescriptor, CapabilityHostApi, CapabilityRegistry, CapabilityRouter,
    };
    use eva_core::{
        CapabilityId, CapabilityName, EventId, EventPayload, GenerationId, InvokeRequest,
        InvokeResponse, RequestId, TraceContext,
    };
    use eva_observability::AuditAction;
    use std::cell::Cell;
    use std::rc::Rc;

    /// 执行 `event` 对应的处理逻辑。
    fn event() -> Event {
        Event::new(
            EventId::parse("evt-1").unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::text("hello"),
        )
    }

    /// 表示 `CancellingToolHost` 数据结构。
    struct CancellingToolHost {
        /// 记录 `token` 字段对应的值。
        token: LuaCancellationToken,
        /// 记录 `calls` 字段对应的值。
        calls: Cell<u64>,
    }

    impl CancellingToolHost {
        /// 创建并初始化当前类型的实例。
        fn new(token: LuaCancellationToken) -> Self {
            Self {
                token,
                calls: Cell::new(0),
            }
        }

        /// 执行 `calls` 对应的受控流程。
        fn calls(&self) -> u64 {
            self.calls.get()
        }
    }

    impl CapabilityHostApi for CancellingToolHost {
        /// 执行 `invoke` 对应的受控流程。
        fn invoke(&self, request: InvokeRequest) -> Result<InvokeResponse, EvaError> {
            self.calls.set(self.calls.get() + 1);
            self.token.cancel();
            Ok(InvokeResponse::completed(
                request.request_id().clone(),
                eva_core::InvokeOutput::text("cancelled after first call"),
            ))
        }
    }

    /// 验证 `on_event_extracts_static_result_fields` 场景下的预期行为。
    #[test]
    fn on_event_extracts_static_result_fields() {
        let script = LuaScript::from_source(
            r#"
function root.on_event(event, ctx)
  return {
    status = "accepted",
    agent_id = "root-agent",
    capability = "config.lint",
    note = "ok",
  }
end
"#,
        );
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap());

        let result = LuaHost::new()
            .run_on_event(&script, &event(), &ctx)
            .unwrap();

        assert_eq!(result.status, "accepted");
        assert_eq!(result.topic.as_str(), "/input/user");
        assert_eq!(result.capability.unwrap().as_str(), "config.lint");
    }

    /// 验证 `on_event_receives_controlled_context_snapshot` 场景下的预期行为。
    #[test]
    fn on_event_receives_controlled_context_snapshot() {
        let script = LuaScript::from_source(
            r#"
function root.on_event(event, ctx)
  return { status = "handled" }
end
"#,
        );
        let snapshot = LuaContextSnapshot {
            private_memory_count: 1,
            global_memory_count: 1,
            knowledge_count: 2,
            audit: vec!["scope:controlled".to_owned()],
        };
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap())
            .with_context(snapshot.clone());

        let result = LuaHost::new()
            .run_on_event(&script, &event(), &ctx)
            .unwrap();

        assert_eq!(result.context, snapshot);
    }

    /// 验证 `on_event_receives_read_only_request_trace_and_memory_tables` 场景下的预期行为。
    #[test]
    fn on_event_receives_read_only_request_trace_and_memory_tables() {
        let script = LuaScript::from_source(
            r#"
local root = {}

function root.on_event(event, ctx)
  return {
    status = ctx.request.request_id .. ":" .. ctx.trace.correlation_id .. ":" .. tostring(ctx.memory.private_memory_count) .. ":" .. tostring(ctx.private_memory_count),
    note = ctx.memory.audit[1],
  }
end

return root
"#,
        );
        let event = event()
            .with_request_id(RequestId::parse("req-lua-context-1").unwrap())
            .with_generation_id(GenerationId::parse("gen-lua-context-1").unwrap())
            .with_trace(TraceContext::new(
                Some(EventId::parse("evt-correlation-1").unwrap()),
                Some(EventId::parse("evt-parent-1").unwrap()),
            ));
        let snapshot = LuaContextSnapshot {
            private_memory_count: 3,
            global_memory_count: 2,
            knowledge_count: 1,
            audit: vec!["scope:controlled".to_owned()],
        };
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap())
            .with_context(snapshot.clone());

        let result = LuaHost::new().run_on_event(&script, &event, &ctx).unwrap();

        assert_eq!(result.status, "req-lua-context-1:evt-correlation-1:3:3");
        assert_eq!(result.note.as_deref(), Some("scope:controlled"));
        assert_eq!(result.context, snapshot);
    }

    /// 验证 `on_event_cannot_mutate_memory_snapshot_table` 场景下的预期行为。
    #[test]
    fn on_event_cannot_mutate_memory_snapshot_table() {
        let script = LuaScript::from_source(
            r#"
local root = {}

function root.on_event(event, ctx)
  ctx.memory.private_memory_count = 99
  return { status = "mutated" }
end

return root
"#,
        );
        let snapshot = LuaContextSnapshot {
            private_memory_count: 1,
            global_memory_count: 0,
            knowledge_count: 0,
            audit: Vec::new(),
        };
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap()).with_context(snapshot);

        let error = LuaHost::new()
            .run_on_event(&script, &event(), &ctx)
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Internal);
        assert_eq!(error.provider_code().unwrap().as_str(), "lua_runtime_error");
    }

    /// 验证 `on_event_cannot_rawset_memory_snapshot_table` 场景下的预期行为。
    #[test]
    fn on_event_cannot_rawset_memory_snapshot_table() {
        let script = LuaScript::from_source(
            r#"
local root = {}

function root.on_event(event, ctx)
  rawset(ctx.memory, "private_memory_count", 99)
  return { status = "mutated" }
end

return root
"#,
        );
        let snapshot = LuaContextSnapshot {
            private_memory_count: 1,
            global_memory_count: 0,
            knowledge_count: 0,
            audit: Vec::new(),
        };
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap()).with_context(snapshot);

        let error = LuaHost::new()
            .run_on_event(&script, &event(), &ctx)
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Internal);
        assert_eq!(error.provider_code().unwrap().as_str(), "lua_runtime_error");
    }

    /// 验证 `on_event_host_log_and_audit_emit_traceable_observability` 场景下的预期行为。
    #[test]
    fn on_event_host_log_and_audit_emit_traceable_observability() {
        let script = LuaScript::from_source(
            r#"
local root = {}

function root.on_event(event, ctx)
  ctx.host.log("info", "handled " .. event.event_id)
  ctx.host.audit("accepted " .. ctx.request.request_id)
  return { status = "observed" }
end

return root
"#,
        );
        let event = event()
            .with_request_id(RequestId::parse("req-lua-observe-1").unwrap())
            .with_generation_id(GenerationId::parse("gen-lua-observe-1").unwrap())
            .with_trace(TraceContext::correlated(
                EventId::parse("evt-observe-correlation").unwrap(),
            ));
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap());

        let result = LuaHost::new().run_on_event(&script, &event, &ctx).unwrap();

        assert_eq!(result.status, "observed");
        assert_eq!(result.observability.len(), 2);
        assert_eq!(result.observability[0].action, AuditAction::LuaHostLog);
        assert_eq!(
            result.observability[0].message.as_deref(),
            Some("handled evt-1")
        );
        assert_eq!(
            result.observability[0].fields[0],
            ("level".to_owned(), "info".to_owned())
        );
        assert_eq!(result.observability[1].action, AuditAction::LuaHostAudit);
        assert_eq!(
            result.observability[1]
                .trace
                .request_id
                .as_ref()
                .unwrap()
                .as_str(),
            "req-lua-observe-1"
        );
        assert_eq!(
            result.observability[1]
                .trace
                .correlation_id
                .as_ref()
                .unwrap()
                .as_str(),
            "evt-observe-correlation"
        );
        assert_eq!(
            result.observability[1]
                .trace
                .agent_id
                .as_ref()
                .unwrap()
                .as_str(),
            "root-agent"
        );
    }

    /// 验证 `on_event_can_call_capability_through_ctx_tools` 场景下的预期行为。
    #[test]
    fn on_event_can_call_capability_through_ctx_tools() {
        let script = LuaScript::from_source(
            r#"
local root = {}

function root.on_event(event, ctx)
  local tool = ctx.tools.call("runtime.echo", { message = event.payload, nested = { 1, true } })
  return {
    status = tool.status,
    note = tool.output,
  }
end

return root
"#,
        );
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap());
        let tool_host: Rc<dyn CapabilityHostApi> = Rc::new(CapabilityRouter::with_v04_builtins());

        let result = LuaHost::new()
            .run_on_event_with_tools(&script, &event(), &ctx, tool_host)
            .unwrap();

        assert_eq!(result.status, "completed");
        let output = result.note.unwrap();
        assert!(output.contains("echo"));
        assert!(output.contains("message"));
        assert!(output.contains("hello"));
    }

    /// 验证 `on_event_tool_calls_use_distinct_request_ids` 场景下的预期行为。
    #[test]
    fn on_event_tool_calls_use_distinct_request_ids() {
        let script = LuaScript::from_source(
            r#"
local root = {}

function root.on_event(event, ctx)
  local first = ctx.tools.call("runtime.echo", "one")
  local second = ctx.tools.call("runtime.echo", "two")
  return {
    status = first.request_id ~= second.request_id and "distinct" or "duplicate",
    note = first.request_id .. ":" .. second.request_id,
  }
end

return root
"#,
        );
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap());
        let tool_host: Rc<dyn CapabilityHostApi> = Rc::new(CapabilityRouter::with_v04_builtins());

        let result = LuaHost::new()
            .run_on_event_with_tools(&script, &event(), &ctx, tool_host)
            .unwrap();

        assert_eq!(result.status, "distinct");
        let note = result.note.unwrap();
        assert!(note.contains(":lua-tool:1:runtime.echo"));
        assert!(note.contains(":lua-tool:2:runtime.echo"));
    }

    /// 验证 `on_event_rejects_unknown_ctx_tool_capability` 场景下的预期行为。
    #[test]
    fn on_event_rejects_unknown_ctx_tool_capability() {
        let script = LuaScript::from_source(
            r#"
local root = {}

function root.on_event(event, ctx)
  ctx.tools.call("runtime.missing", event.payload)
  return { status = "unreachable" }
end

return root
"#,
        );
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap());
        let tool_host: Rc<dyn CapabilityHostApi> = Rc::new(CapabilityRouter::with_v04_builtins());

        let error = LuaHost::new()
            .run_on_event_with_tools(&script, &event(), &ctx, tool_host)
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Internal);
        assert_eq!(error.provider_code().unwrap().as_str(), "lua_runtime_error");
    }

    /// 验证 `on_event_rejects_disabled_ctx_tool_capability` 场景下的预期行为。
    #[test]
    fn on_event_rejects_disabled_ctx_tool_capability() {
        let script = LuaScript::from_source(
            r#"
local root = {}

function root.on_event(event, ctx)
  ctx.tools.call("runtime.echo", event.payload)
  return { status = "unreachable" }
end

return root
"#,
        );
        let mut registry = CapabilityRegistry::new();
        registry
            .register(CapabilityDescriptor {
                id: CapabilityId::parse("runtime-echo-disabled").unwrap(),
                name: CapabilityName::parse("runtime.echo").unwrap(),
                enabled: false,
                provider: "builtin".to_owned(),
                provider_selection: Default::default(),
            })
            .unwrap();
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap());
        let tool_host: Rc<dyn CapabilityHostApi> = Rc::new(CapabilityRouter::new(registry));

        let error = LuaHost::new()
            .run_on_event_with_tools(&script, &event(), &ctx, tool_host)
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Internal);
        assert_eq!(error.provider_code().unwrap().as_str(), "lua_runtime_error");
    }

    /// 验证 `on_event_infinite_loop_is_interrupted_by_timeout_limit` 场景下的预期行为。
    #[test]
    fn on_event_infinite_loop_is_interrupted_by_timeout_limit() {
        let script = LuaScript::from_source(
            r#"
local root = {}

function root.on_event(event, ctx)
  while true do
  end
  return { status = "unreachable" }
end

return root
"#,
        );
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap());
        let limits = LuaExecutionLimits::with_timeout(std::time::Duration::from_millis(1))
            .with_hook_instruction_interval(1);

        let error = LuaHost::new()
            .run_on_event_with_limits(&script, &event(), &ctx, limits)
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Timeout);
        assert_eq!(error.provider_code().unwrap().as_str(), "lua_timeout");
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "timeout_ms" && value == "1"));
    }

    /// 验证 `on_event_infinite_loop_is_interrupted_by_instruction_budget` 场景下的预期行为。
    #[test]
    fn on_event_infinite_loop_is_interrupted_by_instruction_budget() {
        let script = LuaScript::from_source(
            r#"
local root = {}

function root.on_event(event, ctx)
  while true do
  end
  return { status = "unreachable" }
end

return root
"#,
        );
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap());
        let limits =
            LuaExecutionLimits::with_instruction_budget(10).with_hook_instruction_interval(1);

        let error = LuaHost::new()
            .run_on_event_with_limits(&script, &event(), &ctx, limits)
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Timeout);
        assert_eq!(
            error.provider_code().unwrap().as_str(),
            "lua_instruction_budget_exceeded"
        );
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "instruction_budget" && value == "10"));
    }

    /// 验证 `on_event_cancellation_token_stops_before_second_tool_call` 场景下的预期行为。
    #[test]
    fn on_event_cancellation_token_stops_before_second_tool_call() {
        let script = LuaScript::from_source(
            r#"
local root = {}

function root.on_event(event, ctx)
  ctx.tools.call("runtime.echo", "first")
  while true do
  end
  ctx.tools.call("runtime.echo", "second")
  return { status = "unreachable" }
end

return root
"#,
        );
        let token = LuaCancellationToken::new();
        let tool_host = Rc::new(CancellingToolHost::new(token.clone()));
        let limits = LuaExecutionLimits::none()
            .with_cancellation_token(token)
            .with_hook_instruction_interval(1);
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap());

        let error = LuaHost::new()
            .run_on_event_with_tools_and_limits(&script, &event(), &ctx, tool_host.clone(), limits)
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(error.provider_code().unwrap().as_str(), "lua_cancelled");
        assert_eq!(tool_host.calls(), 1);
    }

    /// 验证 `on_event_memory_growth_is_rejected_by_memory_budget` 场景下的预期行为。
    #[test]
    fn on_event_memory_growth_is_rejected_by_memory_budget() {
        let script = LuaScript::from_source(
            r#"
local root = {}

function root.on_event(event, ctx)
  local payload = string.rep("x", 1024 * 1024)
  return { status = payload }
end

return root
"#,
        );
        let limits = LuaExecutionLimits::with_memory_limit_bytes(128 * 1024);
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap());

        let error = LuaHost::new()
            .run_on_event_with_limits(&script, &event(), &ctx, limits)
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Timeout);
        assert_eq!(
            error.provider_code().unwrap().as_str(),
            "lua_memory_limit_exceeded"
        );
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "memory_limit_bytes" && value == "131072"));
    }

    /// 验证 `ctx_tools_exposes_only_call_function` 场景下的预期行为。
    #[test]
    fn ctx_tools_exposes_only_call_function() {
        let script = LuaScript::from_source(
            r#"
local root = {}

function root.on_event(event, ctx)
  local sealed = ctx.tools.call ~= nil
    and ctx.tools.provider == nil
    and ctx.tools.file == nil
    and ctx.tools.socket == nil
    and ctx.tools.process == nil
  return { status = sealed and "sealed" or "leaked" }
end

return root
"#,
        );
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap());
        let tool_host: Rc<dyn CapabilityHostApi> = Rc::new(CapabilityRouter::with_v04_builtins());

        let result = LuaHost::new()
            .run_on_event_with_tools(&script, &event(), &ctx, tool_host)
            .unwrap();

        assert_eq!(result.status, "sealed");
    }

    /// 验证 `real_vm_executes_lua_logic` 场景下的预期行为。
    #[test]
    fn real_vm_executes_lua_logic() {
        let script = LuaScript::from_source(
            r#"
local root = {}

function root.on_event(event, ctx)
  return {
    status = "accepted",
    agent_id = ctx.agent_id,
    topic = event.topic,
    capability = "config.lint",
    capability_input = event.payload,
    note = "handled " .. event.event_id,
  }
end

return root
"#,
        );
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap());

        let result = LuaHost::new()
            .run_on_event(&script, &event(), &ctx)
            .unwrap();

        assert_eq!(result.status, "accepted");
        assert_eq!(result.topic.as_str(), "/input/user");
        assert_eq!(result.capability.unwrap().as_str(), "config.lint");
        assert_eq!(result.capability_input.as_deref(), Some("hello"));
        assert_eq!(result.note.as_deref(), Some("handled evt-1"));
    }

    /// 验证 `real_vm_does_not_load_os_library` 场景下的预期行为。
    #[test]
    fn real_vm_does_not_load_os_library() {
        let script = LuaScript::from_source(
            r#"
local root = {}

function root.on_event(event, ctx)
  return {
    status = os == nil and "restricted" or "leaked",
  }
end

return root
"#,
        );
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap());

        let result = LuaHost::new()
            .run_on_event(&script, &event(), &ctx)
            .unwrap();

        assert_eq!(result.status, "restricted");
    }

    /// 验证 `syntax_error_maps_without_host_path` 场景下的预期行为。
    #[test]
    fn syntax_error_maps_without_host_path() {
        let script = LuaScript::from_source("function root.on_event(event, ctx)");
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap());

        let error = LuaHost::new()
            .run_on_event(&script, &event(), &ctx)
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
        assert_eq!(error.provider_code().unwrap().as_str(), "lua_syntax_error");
        assert!(!error.message().contains(env!("CARGO_MANIFEST_DIR")));
    }

    /// 验证 `runtime_error_maps_without_host_path` 场景下的预期行为。
    #[test]
    fn runtime_error_maps_without_host_path() {
        let script = LuaScript::from_source(
            r#"
local root = {}

function root.on_event(event, ctx)
  error("boom")
end

return root
"#,
        );
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap());

        let error = LuaHost::new()
            .run_on_event(&script, &event(), &ctx)
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Internal);
        assert_eq!(error.provider_code().unwrap().as_str(), "lua_runtime_error");
        assert!(!error.message().contains(env!("CARGO_MANIFEST_DIR")));
    }

    /// 验证 `static_parser_remains_compatibility_fallback` 场景下的预期行为。
    #[test]
    fn static_parser_remains_compatibility_fallback() {
        let script = LuaScript::from_source(
            r#"
legacy on_event table contract
status = "accepted"
capability = "config.lint"
note = "fallback"
"#,
        );
        let ctx = LuaHostContext::new(AgentId::parse("root-agent").unwrap());

        let result = LuaHost::new()
            .run_on_event(&script, &event(), &ctx)
            .unwrap();

        assert_eq!(result.status, "accepted");
        assert_eq!(result.capability.unwrap().as_str(), "config.lint");
        assert_eq!(result.note.as_deref(), Some("fallback"));
    }
}
