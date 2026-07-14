//! 在不切换活动代际的前提下影子加载一组 Lua 脚本。
//!
//! 每个候选先通过沙箱和受限 VM 健康检查，失败按脚本记录并使整代拒绝；报告只描述候选
//! 健康度，不在本模块提交代际切换，从而给上层保留原子提升或回滚的决定权。
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

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "Lua generation swap and rollback boundaries";

/// 表示 `LuaGeneration` 数据结构。
/// Read-only Lua generation marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaGeneration {
    /// 记录 `generation_id` 字段对应的值。
    pub generation_id: GenerationId,
    /// 记录 `script_count` 字段对应的值。
    pub script_count: usize,
}

/// 表示 `LuaShadowCandidate` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaShadowCandidate {
    /// 记录 `agent_id` 字段对应的值。
    pub agent_id: AgentId,
    /// 记录 `script` 字段对应的值。
    pub script: LuaScript,
}

/// 定义 `LuaShadowLoadStatus` 可取的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LuaShadowLoadStatus {
    /// 表示 `Healthy` 枚举分支。
    Healthy,
    /// 表示 `Rejected` 枚举分支。
    Rejected,
}

/// 表示 `LuaShadowScriptReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaShadowScriptReport {
    /// 记录 `agent_id` 字段对应的值。
    pub agent_id: AgentId,
    /// 记录 `status` 字段对应的值。
    pub status: LuaShadowLoadStatus,
    /// 记录 `note` 字段对应的值。
    pub note: Option<String>,
    /// 记录 `error_kind` 字段对应的值。
    pub error_kind: Option<String>,
    /// 记录 `provider_code` 字段对应的值。
    pub provider_code: Option<String>,
    /// 记录 `message` 字段对应的值。
    pub message: Option<String>,
    /// 记录 `observation_count` 字段对应的值。
    pub observation_count: usize,
}

/// 表示 `LuaShadowLoadReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaShadowLoadReport {
    /// 记录 `generation_id` 字段对应的值。
    pub generation_id: GenerationId,
    /// 记录 `script_count` 字段对应的值。
    pub script_count: usize,
    /// 记录 `status` 字段对应的值。
    pub status: LuaShadowLoadStatus,
    /// 记录 `scripts` 字段对应的值。
    pub scripts: Vec<LuaShadowScriptReport>,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

/// 表示 `LuaShadowLoader` 数据结构。
#[derive(Debug, Clone)]
pub struct LuaShadowLoader {
    /// 记录 `host` 字段对应的值。
    host: LuaHost<MluaVmAdapter>,
    /// 记录 `limits` 字段对应的值。
    limits: LuaExecutionLimits,
}

impl LuaGeneration {
    /// 创建并初始化当前类型的实例。
    pub fn new(generation_id: GenerationId, script_count: usize) -> Self {
        Self {
            generation_id,
            script_count,
        }
    }
}

impl LuaShadowCandidate {
    /// 创建并初始化当前类型的实例。
    pub fn new(agent_id: AgentId, script: LuaScript) -> Self {
        Self { agent_id, script }
    }
}

impl LuaShadowLoadStatus {
    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Rejected => "rejected",
        }
    }
}

impl LuaShadowLoader {
    /// 创建并初始化当前类型的实例。
    pub fn new() -> Self {
        Self {
            host: LuaHost::new(),
            limits: default_shadow_limits(),
        }
    }

    /// 设置 `limits` 并返回更新后的实例。
    pub fn with_limits(mut self, limits: LuaExecutionLimits) -> Self {
        self.limits = limits;
        self
    }

    /// 执行 `shadow_load_generation` 对应的处理逻辑。
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
    /// 创建采用默认影子执行限制的加载器。
    fn default() -> Self {
        Self::new()
    }
}

/// 表示 `ShadowCapabilityHost` 数据结构。
#[derive(Debug, Default)]
struct ShadowCapabilityHost;

impl CapabilityHostApi for ShadowCapabilityHost {
    /// 执行 `invoke` 对应的受控流程。
    fn invoke(&self, request: InvokeRequest) -> Result<InvokeResponse, EvaError> {
        Ok(InvokeResponse::completed(
            request.request_id().clone(),
            InvokeOutput::text("{\"shadow\":true}"),
        ))
    }
}

/// 执行 `default_shadow_limits` 对应的处理逻辑。
fn default_shadow_limits() -> LuaExecutionLimits {
    LuaExecutionLimits::with_timeout(Duration::from_millis(250))
        .with_instruction_budget_limit(100_000)
        .with_memory_limit(4 * 1024 * 1024)
        .with_hook_instruction_interval(100)
}

/// 执行 `shadow_health_event` 对应的处理逻辑。
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

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::ErrorKind;

    /// 执行 `generation` 对应的处理逻辑。
    fn generation() -> GenerationId {
        GenerationId::parse("gen-shadow-1").unwrap()
    }

    /// 执行 `agent` 对应的处理逻辑。
    fn agent() -> AgentId {
        AgentId::parse("root-agent").unwrap()
    }

    /// 验证 `shadow_load_reports_healthy_candidate_without_promoting` 场景下的预期行为。
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

    /// 验证 `shadow_load_rejects_forbidden_script_without_switching_generation` 场景下的预期行为。
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
