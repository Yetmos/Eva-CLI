//! Lua VM adapter boundary for executing controlled Agent scripts.

use crate::bindings::{LuaEventResult, LuaHostContext};
use crate::loader::LuaScript;
use eva_core::{AgentId, CapabilityName, EvaError, Event, Topic};
use mlua::{Function, Lua, LuaOptions, StdLib, Table, Value};

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
        let event_table = event_table(&lua, event)?;
        let ctx_table = ctx_table(&lua, ctx)?;
        let result = handler
            .call::<_, Value>((event_table, ctx_table))
            .map_err(map_handler_error)?;

        result_table(result).and_then(|table| lua_result(table, event, ctx))
    }
}

fn controlled_lua() -> Result<Lua, EvaError> {
    Lua::new_with(
        StdLib::TABLE | StdLib::STRING | StdLib::UTF8 | StdLib::MATH,
        LuaOptions::default(),
    )
    .map_err(map_host_setup_error)
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
    let table = lua.create_table().map_err(map_host_setup_error)?;
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
    Ok(table)
}

fn ctx_table<'lua>(lua: &'lua Lua, ctx: &LuaHostContext) -> Result<Table<'lua>, EvaError> {
    let table = lua.create_table().map_err(map_host_setup_error)?;
    table
        .set("agent_id", ctx.agent_id.as_str())
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

    let audit = lua.create_table().map_err(map_host_setup_error)?;
    for (index, entry) in ctx.context.audit.iter().enumerate() {
        audit
            .set(index + 1, entry.as_str())
            .map_err(map_host_setup_error)?;
    }
    table.set("audit", audit).map_err(map_host_setup_error)?;
    Ok(table)
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
