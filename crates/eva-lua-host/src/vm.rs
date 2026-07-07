//! Lua VM adapter boundary for executing controlled Agent scripts.

use crate::bindings::{LuaEventResult, LuaHostContext, LuaHostObservation};
use crate::loader::LuaScript;
use eva_capability::CapabilityHostApi;
use eva_core::{
    AgentId, CapabilityName, EvaError, Event, InvokeInput, InvokeRequest, InvokeStatus,
    InvokeTarget, RequestId, Topic, TraceContext,
};
use eva_observability::{AuditAction, TraceFields};
use mlua::{Function, HookTriggers, Lua, LuaOptions, StdLib, Table, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "execute Lua on_event handlers behind a VM adapter boundary";

const LUA_TIMEOUT_MARKER: &str = "eva_lua_timeout";
const DEFAULT_HOOK_INSTRUCTION_INTERVAL: u32 = 1_000;

/// Execution limits applied inside the Lua VM boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LuaExecutionLimits {
    pub timeout: Option<Duration>,
    pub hook_instruction_interval: u32,
}

impl LuaExecutionLimits {
    pub const fn none() -> Self {
        Self {
            timeout: None,
            hook_instruction_interval: DEFAULT_HOOK_INSTRUCTION_INTERVAL,
        }
    }

    pub const fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout: Some(timeout),
            hook_instruction_interval: DEFAULT_HOOK_INSTRUCTION_INTERVAL,
        }
    }

    pub const fn with_hook_instruction_interval(mut self, interval: u32) -> Self {
        self.hook_instruction_interval = interval;
        self
    }
}

impl Default for LuaExecutionLimits {
    fn default() -> Self {
        Self::none()
    }
}

/// Adapter trait for Lua VM implementations.
pub trait LuaVmAdapter {
    fn run_on_event_with_limits(
        &self,
        script: &LuaScript,
        event: &Event,
        ctx: &LuaHostContext,
        limits: LuaExecutionLimits,
    ) -> Result<LuaEventResult, EvaError>;

    fn run_on_event(
        &self,
        script: &LuaScript,
        event: &Event,
        ctx: &LuaHostContext,
    ) -> Result<LuaEventResult, EvaError> {
        self.run_on_event_with_limits(script, event, ctx, LuaExecutionLimits::default())
    }

    fn run_on_event_with_tools_and_limits(
        &self,
        script: &LuaScript,
        event: &Event,
        ctx: &LuaHostContext,
        tool_host: Rc<dyn CapabilityHostApi>,
        limits: LuaExecutionLimits,
    ) -> Result<LuaEventResult, EvaError> {
        let _ = tool_host;
        self.run_on_event_with_limits(script, event, ctx, limits)
    }

    fn run_on_event_with_tools(
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
}

/// `mlua`-backed VM adapter used by the V1.7.1 execution boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MluaVmAdapter;

impl LuaVmAdapter for MluaVmAdapter {
    fn run_on_event_with_limits(
        &self,
        script: &LuaScript,
        event: &Event,
        ctx: &LuaHostContext,
        limits: LuaExecutionLimits,
    ) -> Result<LuaEventResult, EvaError> {
        self.run_on_event_inner(script, event, ctx, None, limits)
    }

    fn run_on_event_with_tools_and_limits(
        &self,
        script: &LuaScript,
        event: &Event,
        ctx: &LuaHostContext,
        tool_host: Rc<dyn CapabilityHostApi>,
        limits: LuaExecutionLimits,
    ) -> Result<LuaEventResult, EvaError> {
        self.run_on_event_inner(script, event, ctx, Some(tool_host), limits)
    }
}

impl MluaVmAdapter {
    fn run_on_event_inner(
        &self,
        script: &LuaScript,
        event: &Event,
        ctx: &LuaHostContext,
        tool_host: Option<Rc<dyn CapabilityHostApi>>,
        limits: LuaExecutionLimits,
    ) -> Result<LuaEventResult, EvaError> {
        let lua = controlled_lua()?;
        install_execution_limits(&lua, limits)?;
        let root = lua.create_table().map_err(map_host_setup_error)?;
        lua.globals()
            .set("root", root)
            .map_err(map_host_setup_error)?;

        let chunk = lua.load(script.source()).set_name("eva-agent-script");
        let loaded = chunk
            .eval::<Value>()
            .map_err(|error| map_load_error(error, limits))?;
        let handler = on_event_handler(&lua, loaded)?;
        let observations = Rc::new(RefCell::new(Vec::new()));
        let event_table = event_table(&lua, event)?;
        let ctx_table = ctx_table(&lua, event, ctx, Rc::clone(&observations), tool_host)?;
        let result = handler
            .call::<_, Value>((event_table, ctx_table))
            .map_err(|error| map_handler_error(error, limits))?;

        let mut event_result =
            result_table(result).and_then(|table| lua_result(table, event, ctx))?;
        event_result.observability = observations.borrow().clone();
        Ok(event_result)
    }
}

fn controlled_lua() -> Result<Lua, EvaError> {
    let lua = Lua::new_with(
        StdLib::TABLE | StdLib::STRING | StdLib::UTF8 | StdLib::MATH,
        LuaOptions::default(),
    )
    .map_err(map_host_setup_error)?;
    lua.globals()
        .set("rawset", Value::Nil)
        .map_err(map_host_setup_error)?;
    Ok(lua)
}

fn install_execution_limits(lua: &Lua, limits: LuaExecutionLimits) -> Result<(), EvaError> {
    let Some(timeout) = limits.timeout else {
        return Ok(());
    };
    let deadline = Instant::now() + timeout;
    let interval = limits.hook_instruction_interval.max(1);
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(interval),
        move |_, _| {
            if Instant::now() >= deadline {
                return Err(mlua::Error::RuntimeError(LUA_TIMEOUT_MARKER.to_owned()));
            }
            Ok(())
        },
    );
    Ok(())
}

fn lua_timeout_error(limits: LuaExecutionLimits) -> EvaError {
    let timeout_ms = if let Some(timeout) = limits.timeout {
        timeout.as_millis().to_string()
    } else {
        "unknown".to_owned()
    };
    EvaError::timeout("Lua on_event exceeded timeout budget")
        .with_provider_code("lua_timeout")
        .with_context("lua_phase", "handler")
        .with_context("timeout_ms", timeout_ms)
}

fn is_lua_timeout_error(error: &mlua::Error) -> bool {
    match error {
        mlua::Error::RuntimeError(message) => message == LUA_TIMEOUT_MARKER,
        mlua::Error::CallbackError { cause, .. } => is_lua_timeout_error(cause),
        _ => false,
    }
}

fn on_event_handler<'lua>(lua: &'lua Lua, loaded: Value<'lua>) -> Result<Function<'lua>, EvaError> {
    if let Value::Table(table) = loaded {
        if let Some(handler) = table
            .get::<_, Option<Function>>("on_event")
            .map_err(map_result_error)?
        {
            return Ok(handler);
        }
    }

    if let Some(handler) = lua
        .globals()
        .get::<_, Option<Function>>("on_event")
        .map_err(map_result_error)?
    {
        return Ok(handler);
    }

    let root = lua
        .globals()
        .get::<_, Option<Table>>("root")
        .map_err(map_result_error)?;
    if let Some(root) = root {
        if let Some(handler) = root
            .get::<_, Option<Function>>("on_event")
            .map_err(map_result_error)?
        {
            return Ok(handler);
        }
    }

    Err(
        EvaError::invalid_argument("Lua script does not define on_event")
            .with_provider_code("lua_missing_on_event")
            .with_context("lua_phase", "handler_lookup"),
    )
}

fn event_table<'lua>(lua: &'lua Lua, event: &Event) -> Result<Table<'lua>, EvaError> {
    readonly_table(lua, |table| {
        table
            .set("event_id", event.event_id().as_str())
            .map_err(map_host_setup_error)?;
        table
            .set("topic", event.topic().as_str())
            .map_err(map_host_setup_error)?;
        if let Some(payload) = event.payload().as_text() {
            table
                .set("payload", payload)
                .map_err(map_host_setup_error)?;
        } else {
            table
                .set("payload", Value::Nil)
                .map_err(map_host_setup_error)?;
        }
        Ok(())
    })
}

fn ctx_table<'lua>(
    lua: &'lua Lua,
    event: &Event,
    ctx: &LuaHostContext,
    observations: Rc<RefCell<Vec<LuaHostObservation>>>,
    tool_host: Option<Rc<dyn CapabilityHostApi>>,
) -> Result<Table<'lua>, EvaError> {
    readonly_table(lua, |table| {
        table
            .set("agent_id", ctx.agent_id.as_str())
            .map_err(map_host_setup_error)?;
        table
            .set("host", host_table(lua, event, ctx, observations)?)
            .map_err(map_host_setup_error)?;
        table
            .set("tools", tools_table(lua, event, ctx, tool_host)?)
            .map_err(map_host_setup_error)?;
        table
            .set("request", request_table(lua, event)?)
            .map_err(map_host_setup_error)?;
        table
            .set("trace", trace_table(lua, event)?)
            .map_err(map_host_setup_error)?;
        table
            .set("memory", memory_table(lua, ctx)?)
            .map_err(map_host_setup_error)?;

        table
            .set("private_memory_count", ctx.context.private_memory_count)
            .map_err(map_host_setup_error)?;
        table
            .set("global_memory_count", ctx.context.global_memory_count)
            .map_err(map_host_setup_error)?;
        table
            .set("knowledge_count", ctx.context.knowledge_count)
            .map_err(map_host_setup_error)?;
        table
            .set("audit", audit_table(lua, ctx)?)
            .map_err(map_host_setup_error)?;
        Ok(())
    })
}

fn tools_table<'lua>(
    lua: &'lua Lua,
    event: &Event,
    ctx: &LuaHostContext,
    tool_host: Option<Rc<dyn CapabilityHostApi>>,
) -> Result<Table<'lua>, EvaError> {
    readonly_table(lua, |table| {
        let Some(tool_host) = tool_host.clone() else {
            let call_fn = lua
                .create_function(|_, (_capability, _input): (String, Value<'_>)| {
                    Err::<Value<'_>, _>(mlua::Error::RuntimeError(
                        "ctx.tools.call requires a configured CapabilityHostApi".to_owned(),
                    ))
                })
                .map_err(map_host_setup_error)?;
            table.set("call", call_fn).map_err(map_host_setup_error)?;
            return Ok(());
        };

        let request_prefix = event
            .metadata()
            .request_id()
            .map(|request_id| request_id.as_str().to_owned())
            .unwrap_or_else(|| event.event_id().as_str().to_owned());
        let trace = trace_fields(event, ctx);
        let caller = ctx.agent_id.clone();
        let call_index = Rc::new(RefCell::new(0_u64));
        let call_fn = lua
            .create_function(move |lua, (capability, input): (String, Value<'_>)| {
                let capability = CapabilityName::parse(&capability).map_err(map_tool_error)?;
                let request_suffix = {
                    let mut call_index = call_index.borrow_mut();
                    *call_index += 1;
                    *call_index
                };
                let request = InvokeRequest::new(
                    RequestId::parse(&format!(
                        "{}:lua-tool:{}:{}",
                        request_prefix,
                        request_suffix,
                        capability.as_str()
                    ))
                    .map_err(map_tool_error)?,
                    InvokeTarget::Capability(capability),
                    InvokeInput::text(lua_value_to_json(input).map_err(map_tool_error)?),
                )
                .with_metadata(
                    eva_core::InvokeMetadata::new()
                        .with_trace(trace_to_core_trace(&trace))
                        .with_caller(caller.clone()),
                );
                let response = tool_host.invoke(request).map_err(map_tool_error)?;
                tool_response_table(lua, &response)
            })
            .map_err(map_host_setup_error)?;
        table.set("call", call_fn).map_err(map_host_setup_error)?;
        Ok(())
    })
}

fn trace_to_core_trace(trace: &TraceFields) -> TraceContext {
    TraceContext::new(trace.correlation_id.clone(), trace.causation_id.clone())
}

fn lua_value_to_json(value: Value<'_>) -> Result<String, EvaError> {
    match value {
        Value::Nil => Ok("null".to_owned()),
        Value::Boolean(value) => Ok(value.to_string()),
        Value::Integer(value) => Ok(value.to_string()),
        Value::Number(value) if value.is_finite() => Ok(value.to_string()),
        Value::Number(_) => Err(json_value_error("number must be finite")),
        Value::String(value) => Ok(value
            .to_str()
            .map(json_string)
            .map_err(map_lua_value_error)?),
        Value::Table(table) => lua_table_to_json(table),
        _ => Err(json_value_error(
            "value must be nil, boolean, number, string, array, or object",
        )),
    }
}

fn lua_table_to_json(table: Table<'_>) -> Result<String, EvaError> {
    let mut array_entries = BTreeMap::new();
    let mut object_entries = Vec::new();

    for pair in table.pairs::<Value<'_>, Value<'_>>() {
        let (key, value) = pair.map_err(map_lua_value_error)?;
        let value = lua_value_to_json(value)?;
        match key {
            Value::Integer(index) if index > 0 => {
                if array_entries.insert(index, value).is_some() {
                    return Err(json_value_error("array indexes must be unique"));
                }
            }
            Value::String(key) => object_entries.push((
                key.to_str()
                    .map(|value| value.to_string())
                    .map_err(map_lua_value_error)?,
                value,
            )),
            _ => {
                return Err(json_value_error(
                    "object keys must be strings and array keys must be positive integers",
                ))
            }
        }
    }

    if !array_entries.is_empty() && !object_entries.is_empty() {
        return Err(json_value_error("table cannot mix array and object keys"));
    }

    if !array_entries.is_empty() {
        let expected_len = array_entries.len() as i64;
        if array_entries.keys().copied().ne(1..=expected_len) {
            return Err(json_value_error("array indexes must be contiguous from 1"));
        }
        return Ok(format!(
            "[{}]",
            array_entries.into_values().collect::<Vec<_>>().join(",")
        ));
    }

    object_entries.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(format!(
        "{{{}}}",
        object_entries
            .into_iter()
            .map(|(key, value)| format!("{}:{}", json_string(&key), value))
            .collect::<Vec<_>>()
            .join(",")
    ))
}

fn tool_response_table<'lua>(
    lua: &'lua Lua,
    response: &eva_core::InvokeResponse,
) -> mlua::Result<Table<'lua>> {
    let table = lua.create_table()?;
    table.set("request_id", response.request_id().as_str())?;
    table.set("status", invoke_status(response.status()))?;
    table.set("ok", response.is_success())?;
    if let Some(output) = response.output().and_then(|output| output.as_text()) {
        table.set("output", output)?;
    } else {
        table.set("output", Value::Nil)?;
    }
    if let Some(error) = response.error() {
        table.set("error", error.message())?;
        table.set("error_kind", error.kind().as_str())?;
    } else {
        table.set("error", Value::Nil)?;
        table.set("error_kind", Value::Nil)?;
    }
    Ok(table)
}

fn invoke_status(status: InvokeStatus) -> &'static str {
    match status {
        InvokeStatus::Accepted => "accepted",
        InvokeStatus::Completed => "completed",
        InvokeStatus::Failed => "failed",
        InvokeStatus::Cancelled => "cancelled",
        InvokeStatus::Timeout => "timeout",
    }
}

fn json_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            value if value.is_control() => output.push_str(&format!("\\u{:04x}", value as u32)),
            value => output.push(value),
        }
    }
    output.push('"');
    output
}

fn json_value_error(message: impl Into<String>) -> EvaError {
    EvaError::invalid_argument(message)
        .with_provider_code("lua_tool_json_error")
        .with_context("lua_phase", "tool_call")
}

fn map_lua_value_error(_: mlua::Error) -> EvaError {
    json_value_error("Lua value could not be converted to JSON")
}

fn host_table<'lua>(
    lua: &'lua Lua,
    event: &Event,
    ctx: &LuaHostContext,
    observations: Rc<RefCell<Vec<LuaHostObservation>>>,
) -> Result<Table<'lua>, EvaError> {
    readonly_table(lua, |table| {
        let trace = trace_fields(event, ctx);
        let log_trace = trace.clone();
        let log_observations = Rc::clone(&observations);
        let log_fn = lua
            .create_function(move |_, (level, message): (String, String)| {
                log_observations.borrow_mut().push(
                    LuaHostObservation::new(AuditAction::LuaHostLog, log_trace.clone())
                        .with_message(message)
                        .with_field("level", level),
                );
                Ok(())
            })
            .map_err(map_host_setup_error)?;
        table.set("log", log_fn).map_err(map_host_setup_error)?;

        let audit_trace = trace.clone();
        let audit_observations = Rc::clone(&observations);
        let audit_fn = lua
            .create_function(move |_, message: String| {
                audit_observations.borrow_mut().push(
                    LuaHostObservation::new(AuditAction::LuaHostAudit, audit_trace.clone())
                        .with_message(message),
                );
                Ok(())
            })
            .map_err(map_host_setup_error)?;
        table.set("audit", audit_fn).map_err(map_host_setup_error)?;
        Ok(())
    })
}

fn trace_fields(event: &Event, ctx: &LuaHostContext) -> TraceFields {
    TraceFields::from_event(event).with_agent_id(ctx.agent_id.clone())
}

fn request_table<'lua>(lua: &'lua Lua, event: &Event) -> Result<Table<'lua>, EvaError> {
    readonly_table(lua, |table| {
        table
            .set("event_id", event.event_id().as_str())
            .map_err(map_host_setup_error)?;
        table
            .set("topic", event.topic().as_str())
            .map_err(map_host_setup_error)?;
        if let Some(request_id) = event.metadata().request_id() {
            table
                .set("request_id", request_id.as_str())
                .map_err(map_host_setup_error)?;
        }
        if let Some(generation_id) = event.metadata().generation_id() {
            table
                .set("generation_id", generation_id.as_str())
                .map_err(map_host_setup_error)?;
        }
        Ok(())
    })
}

fn trace_table<'lua>(lua: &'lua Lua, event: &Event) -> Result<Table<'lua>, EvaError> {
    readonly_table(lua, |table| {
        if let Some(correlation_id) = event.metadata().trace().correlation_id() {
            table
                .set("correlation_id", correlation_id.as_str())
                .map_err(map_host_setup_error)?;
        }
        if let Some(causation_id) = event.metadata().trace().causation_id() {
            table
                .set("causation_id", causation_id.as_str())
                .map_err(map_host_setup_error)?;
        }
        Ok(())
    })
}

fn memory_table<'lua>(lua: &'lua Lua, ctx: &LuaHostContext) -> Result<Table<'lua>, EvaError> {
    readonly_table(lua, |table| {
        table
            .set("private_memory_count", ctx.context.private_memory_count)
            .map_err(map_host_setup_error)?;
        table
            .set("global_memory_count", ctx.context.global_memory_count)
            .map_err(map_host_setup_error)?;
        table
            .set("knowledge_count", ctx.context.knowledge_count)
            .map_err(map_host_setup_error)?;
        table
            .set("audit", audit_table(lua, ctx)?)
            .map_err(map_host_setup_error)?;
        Ok(())
    })
}

fn audit_table<'lua>(lua: &'lua Lua, ctx: &LuaHostContext) -> Result<Table<'lua>, EvaError> {
    readonly_table(lua, |table| {
        for (index, entry) in ctx.context.audit.iter().enumerate() {
            table
                .set(index + 1, entry.as_str())
                .map_err(map_host_setup_error)?;
        }
        Ok(())
    })
}

fn readonly_table<'lua, F>(lua: &'lua Lua, populate: F) -> Result<Table<'lua>, EvaError>
where
    F: FnOnce(&Table<'lua>) -> Result<(), EvaError>,
{
    let data = lua.create_table().map_err(map_host_setup_error)?;
    populate(&data)?;

    let proxy = lua.create_table().map_err(map_host_setup_error)?;
    let metatable = lua.create_table().map_err(map_host_setup_error)?;
    metatable
        .set("__index", data)
        .map_err(map_host_setup_error)?;
    let readonly_error = lua
        .create_function(
            |_, (_table, _key, _value): (Value<'_>, Value<'_>, Value<'_>)| {
                Err::<(), _>(mlua::Error::RuntimeError(
                    "Eva Lua context table is read-only".to_owned(),
                ))
            },
        )
        .map_err(map_host_setup_error)?;
    metatable
        .set("__newindex", readonly_error)
        .map_err(map_host_setup_error)?;
    metatable
        .set("__metatable", "eva_read_only")
        .map_err(map_host_setup_error)?;
    proxy.set_metatable(Some(metatable));
    Ok(proxy)
}

fn result_table(result: Value<'_>) -> Result<Table<'_>, EvaError> {
    match result {
        Value::Table(table) => Ok(table),
        _ => Err(
            EvaError::invalid_argument("Lua on_event returned an invalid result")
                .with_provider_code("lua_result_type_error")
                .with_context("lua_phase", "result"),
        ),
    }
}

fn lua_result(
    table: Table<'_>,
    event: &Event,
    ctx: &LuaHostContext,
) -> Result<LuaEventResult, EvaError> {
    let agent_id = table
        .get::<_, Option<String>>("agent_id")
        .map_err(map_result_error)?
        .map(|value| AgentId::parse(&value))
        .transpose()?
        .unwrap_or_else(|| ctx.agent_id.clone());
    let status = table
        .get::<_, Option<String>>("status")
        .map_err(map_result_error)?
        .unwrap_or_else(|| "handled".to_owned());
    let topic = table
        .get::<_, Option<String>>("topic")
        .map_err(map_result_error)?
        .map(|value| Topic::parse(&value))
        .transpose()?
        .unwrap_or_else(|| event.topic().clone());
    let note = table
        .get::<_, Option<String>>("note")
        .map_err(map_result_error)?;
    let capability = table
        .get::<_, Option<String>>("capability")
        .map_err(map_result_error)?
        .map(|value| CapabilityName::parse(&value))
        .transpose()?;
    let capability_input = table
        .get::<_, Option<String>>("capability_input")
        .map_err(map_result_error)?
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

fn map_host_setup_error(_: mlua::Error) -> EvaError {
    EvaError::internal("Lua VM host setup failed")
        .with_provider_code("lua_host_setup_error")
        .with_context("lua_phase", "host_setup")
}

fn map_load_error(error: mlua::Error, limits: LuaExecutionLimits) -> EvaError {
    if is_lua_timeout_error(&error) {
        return lua_timeout_error(limits).with_context("lua_phase", "load");
    }
    match error {
        mlua::Error::SyntaxError { .. } => {
            EvaError::invalid_argument("Lua script failed to compile")
                .with_provider_code("lua_syntax_error")
                .with_context("lua_phase", "compile")
        }
        _ => EvaError::invalid_argument("Lua script failed during load")
            .with_provider_code("lua_load_error")
            .with_context("lua_phase", "load"),
    }
}

fn map_handler_error(error: mlua::Error, limits: LuaExecutionLimits) -> EvaError {
    if is_lua_timeout_error(&error) {
        return lua_timeout_error(limits);
    }
    EvaError::internal("Lua on_event runtime error")
        .with_provider_code("lua_runtime_error")
        .with_context("lua_phase", "handler")
}

fn map_tool_error(error: EvaError) -> mlua::Error {
    mlua::Error::RuntimeError(format!("Eva tool call failed: {}", error.message()))
}

fn map_result_error(_: mlua::Error) -> EvaError {
    EvaError::invalid_argument("Lua on_event returned an invalid result")
        .with_provider_code("lua_result_error")
        .with_context("lua_phase", "result")
}
