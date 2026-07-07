//! Lua VM adapter boundary for executing controlled Agent scripts.

use crate::bindings::{LuaEventResult, LuaHostContext, LuaHostObservation};
use crate::loader::LuaScript;
use eva_core::{AgentId, CapabilityName, EvaError, Event, Topic};
use eva_observability::{AuditAction, TraceFields};
use mlua::{Function, Lua, LuaOptions, StdLib, Table, Value};
use std::cell::RefCell;
use std::rc::Rc;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "execute Lua on_event handlers behind a VM adapter boundary";

/// Adapter trait for Lua VM implementations.
pub trait LuaVmAdapter {
    fn run_on_event(
        &self,
        script: &LuaScript,
        event: &Event,
        ctx: &LuaHostContext,
    ) -> Result<LuaEventResult, EvaError>;
}

/// `mlua`-backed VM adapter used by the V1.7.1 execution boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MluaVmAdapter;

impl LuaVmAdapter for MluaVmAdapter {
    fn run_on_event(
        &self,
        script: &LuaScript,
        event: &Event,
        ctx: &LuaHostContext,
    ) -> Result<LuaEventResult, EvaError> {
        let lua = controlled_lua()?;
        let root = lua.create_table().map_err(map_host_setup_error)?;
        lua.globals()
            .set("root", root)
            .map_err(map_host_setup_error)?;

        let chunk = lua.load(script.source()).set_name("eva-agent-script");
        let loaded = chunk.eval::<Value>().map_err(map_load_error)?;
        let handler = on_event_handler(&lua, loaded)?;
        let observations = Rc::new(RefCell::new(Vec::new()));
        let event_table = event_table(&lua, event)?;
        let ctx_table = ctx_table(&lua, event, ctx, Rc::clone(&observations))?;
        let result = handler
            .call::<_, Value>((event_table, ctx_table))
            .map_err(map_handler_error)?;

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
) -> Result<Table<'lua>, EvaError> {
    readonly_table(lua, |table| {
        table
            .set("agent_id", ctx.agent_id.as_str())
            .map_err(map_host_setup_error)?;
        table
            .set("host", host_table(lua, event, ctx, observations)?)
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

fn map_load_error(error: mlua::Error) -> EvaError {
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

fn map_handler_error(_: mlua::Error) -> EvaError {
    EvaError::internal("Lua on_event runtime error")
        .with_provider_code("lua_runtime_error")
        .with_context("lua_phase", "handler")
}

fn map_result_error(_: mlua::Error) -> EvaError {
    EvaError::invalid_argument("Lua on_event returned an invalid result")
        .with_provider_code("lua_result_error")
        .with_context("lua_phase", "result")
}
