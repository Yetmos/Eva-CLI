//! Typed host bindings exposed to the controlled Lua contract.

use crate::loader::LuaScript;
use crate::sandbox::LuaSandboxPolicy;
use eva_core::{AgentId, CapabilityName, EvaError, Event, Topic};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "typed host API bindings exposed to Lua";

/// Context passed to a Lua `on_event` handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaHostContext {
    pub agent_id: AgentId,
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
}

/// Synchronous controlled Lua host facade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaHost {
    sandbox: LuaSandboxPolicy,
}

impl LuaHost {
    pub fn new() -> Self {
        Self {
            sandbox: LuaSandboxPolicy::default(),
        }
    }

    pub fn run_on_event(
        &self,
        script: &LuaScript,
        event: &Event,
        ctx: &LuaHostContext,
    ) -> Result<LuaEventResult, EvaError> {
        self.sandbox.validate(script)?;
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
        })
    }
}

impl Default for LuaHost {
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
    use eva_core::{EventId, EventPayload};

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
        let ctx = LuaHostContext {
            agent_id: AgentId::parse("root-agent").unwrap(),
        };

        let result = LuaHost::new()
            .run_on_event(&script, &event(), &ctx)
            .unwrap();

        assert_eq!(result.status, "accepted");
        assert_eq!(result.topic.as_str(), "/input/user");
        assert_eq!(result.capability.unwrap().as_str(), "config.lint");
    }
}
