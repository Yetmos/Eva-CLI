//! Lua generation swap and rollback boundaries.

use crate::bindings::{LuaHost, LuaHostContext};
use crate::loader::LuaScript;
use crate::vm::{LuaExecutionLimits, MluaVmAdapter};
use eva_capability::CapabilityHostApi;
use eva_core::{
    AgentId, EvaError, Event, EventId, EventPayload, EventTarget, GenerationId, InvokeOutput,
    InvokeRequest, InvokeResponse, RequestId, Topic,
};
use std::rc::Rc;
use std::time::Duration;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Lua generation swap and rollback boundaries";

/// Read-only Lua generation marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaGeneration {
    pub generation_id: GenerationId,
    pub script_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaShadowCandidate {
    pub agent_id: AgentId,
    pub script: LuaScript,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LuaShadowLoadStatus {
    Healthy,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaShadowScriptReport {
    pub agent_id: AgentId,
    pub status: LuaShadowLoadStatus,
    pub note: Option<String>,
    pub error_kind: Option<String>,
    pub provider_code: Option<String>,
    pub message: Option<String>,
    pub observation_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaShadowLoadReport {
    pub generation_id: GenerationId,
    pub script_count: usize,
    pub status: LuaShadowLoadStatus,
    pub scripts: Vec<LuaShadowScriptReport>,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LuaShadowLoader {
    host: LuaHost<MluaVmAdapter>,
    limits: LuaExecutionLimits,
}

impl LuaGeneration {
    pub fn new(generation_id: GenerationId, script_count: usize) -> Self {
        Self {
            generation_id,
            script_count,
        }
    }
}

impl LuaShadowCandidate {
    pub fn new(agent_id: AgentId, script: LuaScript) -> Self {
        Self { agent_id, script }
    }
}

impl LuaShadowLoadStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Rejected => "rejected",
        }
    }
}

impl LuaShadowLoader {
    pub fn new() -> Self {
        Self {
            host: LuaHost::new(),
            limits: default_shadow_limits(),
        }
    }

    pub fn with_limits(mut self, limits: LuaExecutionLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn shadow_load_generation(
        &self,
        generation_id: GenerationId,
        candidates: &[LuaShadowCandidate],
    ) -> Result<LuaShadowLoadReport, EvaError> {
        if candidates.is_empty() {
            return Err(
                EvaError::invalid_argument("shadow load requires at least one Lua script")
                    .with_context("generation", generation_id.as_str()),
            );
        }

        let tool_host: Rc<dyn CapabilityHostApi> = Rc::new(ShadowCapabilityHost);
        let mut scripts = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let event = shadow_health_event(&generation_id, &candidate.agent_id);
            let ctx = LuaHostContext::new(candidate.agent_id.clone());
            let result = self.host.run_on_event_with_tools_and_limits(
                &candidate.script,
                &event,
                &ctx,
                Rc::clone(&tool_host),
                self.limits.clone(),
            );
            scripts.push(match result {
                Ok(result) => LuaShadowScriptReport {
                    agent_id: candidate.agent_id.clone(),
                    status: LuaShadowLoadStatus::Healthy,
                    note: result.note,
                    error_kind: None,
                    provider_code: None,
                    message: None,
                    observation_count: result.observability.len(),
                },
                Err(error) => LuaShadowScriptReport {
                    agent_id: candidate.agent_id.clone(),
                    status: LuaShadowLoadStatus::Rejected,
                    note: None,
                    error_kind: Some(error.kind().as_str().to_owned()),
                    provider_code: error.provider_code().map(|code| code.as_str().to_owned()),
                    message: Some(error.message().to_owned()),
                    observation_count: 0,
                },
            });
        }

        let status = if scripts
            .iter()
            .any(|script| script.status == LuaShadowLoadStatus::Rejected)
        {
            LuaShadowLoadStatus::Rejected
        } else {
            LuaShadowLoadStatus::Healthy
        };
        let audit_status = match status {
            LuaShadowLoadStatus::Healthy => "healthy",
            LuaShadowLoadStatus::Rejected => "rejected",
        };
        Ok(LuaShadowLoadReport {
            generation_id: generation_id.clone(),
            script_count: scripts.len(),
            status,
            scripts,
            audit: vec![
                format!("shadow_load:{}:started", generation_id.as_str()),
                format!("shadow_load:{}:{audit_status}", generation_id.as_str()),
            ],
        })
    }
}

impl Default for LuaShadowLoader {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
struct ShadowCapabilityHost;

impl CapabilityHostApi for ShadowCapabilityHost {
    fn invoke(&self, request: InvokeRequest) -> Result<InvokeResponse, EvaError> {
        Ok(InvokeResponse::completed(
            request.request_id().clone(),
            InvokeOutput::text("{\"shadow\":true}"),
        ))
    }
}

fn default_shadow_limits() -> LuaExecutionLimits {
    LuaExecutionLimits::with_timeout(Duration::from_millis(250))
        .with_instruction_budget_limit(100_000)
        .with_memory_limit(4 * 1024 * 1024)
        .with_hook_instruction_interval(100)
}

fn shadow_health_event(generation_id: &GenerationId, agent_id: &AgentId) -> Event {
    Event::new(
        EventId::parse("evt-shadow-health").expect("static shadow health event id is valid"),
        Topic::parse("/runtime/lua/health").expect("static shadow health topic is valid"),
        EventPayload::text("shadow health"),
    )
    .with_request_id(
        RequestId::parse("req-shadow-health").expect("static shadow health request id is valid"),
    )
    .with_generation_id(generation_id.clone())
    .with_target(EventTarget::Agent(agent_id.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::ErrorKind;

    fn generation() -> GenerationId {
        GenerationId::parse("gen-shadow-1").unwrap()
    }

    fn agent() -> AgentId {
        AgentId::parse("root-agent").unwrap()
    }

    #[test]
    fn shadow_load_reports_healthy_candidate_without_promoting() {
        let script = LuaScript::from_source(
            r#"
            local root = {}
            function root.on_event(event, ctx)
              local response = ctx.tools.call("config.lint", "shadow")
              return {
                status = "accepted",
                topic = event.topic,
                note = "shadow " .. response.status
              }
            end
            return root
            "#,
        );
        let candidates = vec![LuaShadowCandidate::new(agent(), script)];

        let report = LuaShadowLoader::new()
            .shadow_load_generation(generation(), &candidates)
            .unwrap();

        assert_eq!(report.status, LuaShadowLoadStatus::Healthy);
        assert_eq!(report.script_count, 1);
        assert_eq!(report.scripts[0].status, LuaShadowLoadStatus::Healthy);
        assert_eq!(report.scripts[0].note.as_deref(), Some("shadow completed"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "shadow_load:gen-shadow-1:healthy"));
        assert!(!report.audit.iter().any(|item| item.contains("promoted")));
    }

    #[test]
    fn shadow_load_rejects_forbidden_script_without_switching_generation() {
        let script = LuaScript::from_source(
            r#"
            local root = {}
            function root.on_event(event, ctx)
              os.execute("rm -rf .")
              return { status = "accepted", topic = event.topic }
            end
            return root
            "#,
        );
        let candidates = vec![LuaShadowCandidate::new(agent(), script)];

        let report = LuaShadowLoader::new()
            .shadow_load_generation(generation(), &candidates)
            .unwrap();

        assert_eq!(report.status, LuaShadowLoadStatus::Rejected);
        assert_eq!(report.scripts[0].status, LuaShadowLoadStatus::Rejected);
        assert_eq!(
            report.scripts[0].error_kind.as_deref(),
            Some(ErrorKind::PermissionDenied.as_str())
        );
        assert!(report
            .audit
            .iter()
            .any(|item| item == "shadow_load:gen-shadow-1:rejected"));
        assert!(!report.audit.iter().any(|item| item.contains("promoted")));
    }
}
