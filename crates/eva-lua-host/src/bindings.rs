//! Typed host bindings exposed to the controlled Lua contract.

use crate::loader::LuaScript;
use crate::sandbox::LuaSandboxPolicy;
use crate::vm::{LuaVmAdapter, MluaVmAdapter};
use eva_core::{AgentId, CapabilityName, EvaError, Event, Topic};
use eva_memory::LuaContextSnapshot;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "typed host API bindings exposed to Lua";

/// Context passed to a Lua `on_event` handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaHostContext {
    pub agent_id: AgentId,
    pub context: LuaContextSnapshot,
}

/// Controlled result returned by the V0.4 Lua host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaEventResult {
    pub agent_id: AgentId,
    pub status: String,
    pub topic: Topic,
    pub note: Option<String>,
    pub capability: Option<CapabilityName>,
    pub capability_input: Option<String>,
    pub context: LuaContextSnapshot,
}

/// Synchronous controlled Lua host facade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaHost<A = MluaVmAdapter> {
    sandbox: LuaSandboxPolicy,
    vm: A,
}

impl LuaHost<MluaVmAdapter> {
    pub fn new() -> Self {
        Self::with_vm_adapter(MluaVmAdapter)
    }
}

impl<A> LuaHost<A> {
    pub fn with_vm_adapter(vm: A) -> Self {
        Self {
            sandbox: LuaSandboxPolicy::default(),
            vm,
        }
    }
}

impl<A: LuaVmAdapter> LuaHost<A> {
    pub fn run_on_event(
        &self,
        script: &LuaScript,
        event: &Event,
        ctx: &LuaHostContext,
    ) -> Result<LuaEventResult, EvaError> {
        self.sandbox.validate(script)?;
        match self.vm.run_on_event(script, event, ctx) {
            Ok(result) => Ok(result),
            Err(error) if should_attempt_static_fallback(script.source(), &error) => {
                parse_static_on_event(script, event, ctx)
            }
            Err(error) => Err(error),
        }
    }
}

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
    })
}

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
    pub fn new(agent_id: AgentId) -> Self {
        Self {
            agent_id,
            context: LuaContextSnapshot::default(),
        }
    }

    pub fn with_context(mut self, context: LuaContextSnapshot) -> Self {
        self.context = context;
        self
    }
}

impl Default for LuaHost<MluaVmAdapter> {
    fn default() -> Self {
        Self::new()
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::{EventId, EventPayload, GenerationId, RequestId, TraceContext};

    fn event() -> Event {
        Event::new(
            EventId::parse("evt-1").unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::text("hello"),
        )
    }

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
