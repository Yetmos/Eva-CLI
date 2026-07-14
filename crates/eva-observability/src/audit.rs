//! 中文：审计事件字段契约、动作枚举和可替换写入端接口。
//! Audit event sink traits and audit field contracts.

use crate::TraceFields;
use eva_core::EvaError;
use std::time::SystemTime;

/// 中文：审计记录使用的稳定动作类别。
/// Stable action categories for audit records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AuditAction {
    /// 中文：项目配置已经成功加载。
    ConfigLoaded,
    /// 中文：项目配置完成语义校验。
    ConfigValidated,
    /// 中文：策略请求已经产生允许或拒绝判定。
    PolicyEvaluated,
    /// 中文：运行时完成启动。
    RuntimeStarted,
    /// 中文：运行时从持久化状态完成恢复。
    RuntimeRecovered,
    /// 中文：运行时处理了控制面请求。
    RuntimeControl,
    /// 中文：运行时完成停止。
    RuntimeStopped,
    /// 中文：恢复计划已经实际应用。
    RestoreApply,
    /// 中文：恢复应用已经执行回滚。
    RestoreRollback,
    /// 中文：调度器执行了死信重试 Tick。
    SchedulerRetry,
    /// 中文：事件已跨越接收边界。
    EventAccepted,
    /// 中文：事件已经投递到消费者。
    EventDelivered,
    /// 中文：任务生命周期发生状态变化。
    TaskLifecycle,
    /// 中文：Lua 宿主写入普通日志。
    LuaHostLog,
    /// 中文：Lua 宿主显式写入审计事件。
    LuaHostAudit,
    /// 中文：Capability 路由完成一次调用。
    CapabilityInvoked,
    /// 中文：Adapter 执行了一次调用。
    AdapterInvoked,
    /// 中文：Provider Supervisor 发生监控或恢复动作。
    ProviderSupervised,
    /// 中文：建立或使用了 Provider 凭据会话。
    ProviderCredentialSession,
    /// 中文：Skill 开始执行。
    SkillRunStarted,
    /// 中文：Skill 成功完成。
    SkillRunCompleted,
    /// 中文：Skill 执行失败。
    SkillRunFailed,
    /// 中文：MCP 会话已经启动。
    McpSessionStarted,
    /// 中文：MCP 会话已经停止。
    McpSessionStopped,
    /// 中文：MCP 流在完成前被中止。
    McpStreamAborted,
    /// 中文：MCP 代理请求被策略拒绝。
    McpProxyDenied,
    /// 中文：记忆记录已写入。
    MemoryWrite,
    /// 中文：记忆记录已读取。
    MemoryRead,
    /// 中文：记忆或知识检索已执行。
    MemorySearch,
    /// 中文：上下文窗口已经构建。
    MemoryContext,
    /// 中文：记忆维护或清理任务已经执行。
    MemoryMaintenance,
    /// 中文：硬件驱动已经启动。
    HardwareDriverStarted,
    /// 中文：硬件驱动已经停止。
    HardwareDriverStopped,
    /// 中文：硬件热插拔事件已经发布。
    HardwareHotplugPublished,
    /// 中文：安全或权限边界拒绝了操作。
    SecurityDenied,
}

/// 中文：审计动作使用的稳定结果分类。
/// Stable audit outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AuditOutcome {
    /// 中文：动作成功完成。
    Ok,
    /// 中文：动作只完成计划或预演，未产生真实副作用。
    Planned,
    /// 中文：动作被策略或前置条件阻止。
    Blocked,
    /// 中文：动作开始后以错误结束。
    Failed,
}

/// 中文：可写入任意后端的完整审计记录。
/// Audit record that can be written to any future backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    /// 中文：发生的稳定动作类别。
    pub action: AuditAction,
    /// 中文：动作的归一化结果。
    pub outcome: AuditOutcome,
    /// 中文：关联请求、事件、Agent 或 Provider 的追踪字段。
    pub trace: TraceFields,
    /// 中文：可选的人类可读说明，不应包含秘密原文。
    pub message: Option<String>,
    /// 中文：按追加顺序保存的扩展键值字段。
    pub fields: Vec<(String, String)>,
    /// 中文：记录在当前进程中创建的系统时间。
    pub recorded_at: SystemTime,
}

/// 中文：由运行时、测试或存储后端实现的审计写入接口。
/// Sink trait implemented by runtime, tests, or storage-backed audit writers.
pub trait AuditSink {
    /// 中文：写入一条完整审计事件；后端失败必须向调用方返回结构化错误。
    fn record(&mut self, event: AuditEvent) -> Result<(), EvaError>;
}

/// 中文：测试和预演流程使用的内存审计写入端。
/// In-memory sink useful for tests and dry-run flows.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryAuditSink {
    /// 中文：按写入顺序保留的全部审计事件。
    pub events: Vec<AuditEvent>,
}

impl AuditAction {
    /// 中文：返回跨文件、数据库和协议保持稳定的动作名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConfigLoaded => "config.loaded",
            Self::ConfigValidated => "config.validated",
            Self::PolicyEvaluated => "policy.evaluated",
            Self::RuntimeStarted => "runtime.started",
            Self::RuntimeRecovered => "runtime.recovered",
            Self::RuntimeControl => "runtime.control",
            Self::RuntimeStopped => "runtime.stopped",
            Self::RestoreApply => "restore.apply",
            Self::RestoreRollback => "restore.rollback",
            Self::SchedulerRetry => "scheduler.retry",
            Self::EventAccepted => "event.accepted",
            Self::EventDelivered => "event.delivered",
            Self::TaskLifecycle => "task.lifecycle",
            Self::LuaHostLog => "lua.host.log",
            Self::LuaHostAudit => "lua.host.audit",
            Self::CapabilityInvoked => "capability.invoked",
            Self::AdapterInvoked => "adapter.invoked",
            Self::ProviderSupervised => "provider.supervised",
            Self::ProviderCredentialSession => "provider.credential_session",
            Self::SkillRunStarted => "skill.run.started",
            Self::SkillRunCompleted => "skill.run.completed",
            Self::SkillRunFailed => "skill.run.failed",
            Self::McpSessionStarted => "mcp.session.started",
            Self::McpSessionStopped => "mcp.session.stopped",
            Self::McpStreamAborted => "mcp.stream.aborted",
            Self::McpProxyDenied => "mcp.proxy.denied",
            Self::MemoryWrite => "memory.write",
            Self::MemoryRead => "memory.read",
            Self::MemorySearch => "memory.search",
            Self::MemoryContext => "memory.context",
            Self::MemoryMaintenance => "memory.maintenance",
            Self::HardwareDriverStarted => "hardware.driver.started",
            Self::HardwareDriverStopped => "hardware.driver.stopped",
            Self::HardwareHotplugPublished => "hardware.hotplug.published",
            Self::SecurityDenied => "security.denied",
        }
    }

    /// 中文：从稳定名称恢复动作；未知名称返回 `None` 以支持兼容读取。
    pub fn from_stable_name(value: &str) -> Option<Self> {
        match value {
            "config.loaded" => Some(Self::ConfigLoaded),
            "config.validated" => Some(Self::ConfigValidated),
            "policy.evaluated" => Some(Self::PolicyEvaluated),
            "runtime.started" => Some(Self::RuntimeStarted),
            "runtime.recovered" => Some(Self::RuntimeRecovered),
            "runtime.control" => Some(Self::RuntimeControl),
            "runtime.stopped" => Some(Self::RuntimeStopped),
            "restore.apply" => Some(Self::RestoreApply),
            "restore.rollback" => Some(Self::RestoreRollback),
            "scheduler.retry" => Some(Self::SchedulerRetry),
            "event.accepted" => Some(Self::EventAccepted),
            "event.delivered" => Some(Self::EventDelivered),
            "task.lifecycle" => Some(Self::TaskLifecycle),
            "lua.host.log" => Some(Self::LuaHostLog),
            "lua.host.audit" => Some(Self::LuaHostAudit),
            "capability.invoked" => Some(Self::CapabilityInvoked),
            "adapter.invoked" => Some(Self::AdapterInvoked),
            "provider.supervised" => Some(Self::ProviderSupervised),
            "provider.credential_session" => Some(Self::ProviderCredentialSession),
            "skill.run.started" => Some(Self::SkillRunStarted),
            "skill.run.completed" => Some(Self::SkillRunCompleted),
            "skill.run.failed" => Some(Self::SkillRunFailed),
            "mcp.session.started" => Some(Self::McpSessionStarted),
            "mcp.session.stopped" => Some(Self::McpSessionStopped),
            "mcp.stream.aborted" => Some(Self::McpStreamAborted),
            "mcp.proxy.denied" => Some(Self::McpProxyDenied),
            "memory.write" => Some(Self::MemoryWrite),
            "memory.read" => Some(Self::MemoryRead),
            "memory.search" => Some(Self::MemorySearch),
            "memory.context" => Some(Self::MemoryContext),
            "memory.maintenance" => Some(Self::MemoryMaintenance),
            "hardware.driver.started" => Some(Self::HardwareDriverStarted),
            "hardware.driver.stopped" => Some(Self::HardwareDriverStopped),
            "hardware.hotplug.published" => Some(Self::HardwareHotplugPublished),
            "security.denied" => Some(Self::SecurityDenied),
            _ => None,
        }
    }
}

impl AuditOutcome {
    /// 中文：返回持久化和展示使用的稳定结果名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Planned => "planned",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
        }
    }
}

impl AuditEvent {
    /// 中文：使用当前系统时间创建没有消息和扩展字段的审计事件。
    pub fn new(action: AuditAction, outcome: AuditOutcome, trace: TraceFields) -> Self {
        Self {
            action,
            outcome,
            trace,
            message: None,
            fields: Vec::new(),
            recorded_at: SystemTime::now(),
        }
    }

    /// 中文：附加人类可读消息。
    pub fn with_message(mut self, value: impl Into<String>) -> Self {
        self.message = Some(value.into());
        self
    }

    /// 中文：按调用顺序追加一个扩展审计字段，不覆盖同名历史值。
    pub fn with_field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.push((key.into(), value.into()));
        self
    }
}

impl AuditSink for InMemoryAuditSink {
    /// 中文：把事件追加到内存列表；该实现不会产生外部失败。
    fn record(&mut self, event: AuditEvent) -> Result<(), EvaError> {
        self.events.push(event);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SpanId;

    #[test]
    /// 中文：验证关键动作和结果名称在序列化边界保持稳定且可反向解析。
    fn audit_action_spelling_is_stable() {
        assert_eq!(AuditAction::ConfigValidated.as_str(), "config.validated");
        assert_eq!(AuditAction::RuntimeRecovered.as_str(), "runtime.recovered");
        assert_eq!(AuditAction::RuntimeControl.as_str(), "runtime.control");
        assert_eq!(AuditAction::RestoreApply.as_str(), "restore.apply");
        assert_eq!(AuditAction::RestoreRollback.as_str(), "restore.rollback");
        assert_eq!(AuditAction::SchedulerRetry.as_str(), "scheduler.retry");
        assert_eq!(AuditAction::TaskLifecycle.as_str(), "task.lifecycle");
        assert_eq!(AuditAction::LuaHostLog.as_str(), "lua.host.log");
        assert_eq!(AuditAction::LuaHostAudit.as_str(), "lua.host.audit");
        assert_eq!(
            AuditAction::ProviderSupervised.as_str(),
            "provider.supervised"
        );
        assert_eq!(
            AuditAction::ProviderCredentialSession.as_str(),
            "provider.credential_session"
        );
        assert_eq!(AuditAction::SkillRunStarted.as_str(), "skill.run.started");
        assert_eq!(
            AuditAction::SkillRunCompleted.as_str(),
            "skill.run.completed"
        );
        assert_eq!(AuditAction::SkillRunFailed.as_str(), "skill.run.failed");
        assert_eq!(
            AuditAction::McpSessionStarted.as_str(),
            "mcp.session.started"
        );
        assert_eq!(
            AuditAction::McpSessionStopped.as_str(),
            "mcp.session.stopped"
        );
        assert_eq!(AuditAction::McpStreamAborted.as_str(), "mcp.stream.aborted");
        assert_eq!(AuditAction::McpProxyDenied.as_str(), "mcp.proxy.denied");
        assert_eq!(AuditAction::MemoryWrite.as_str(), "memory.write");
        assert_eq!(AuditAction::MemoryRead.as_str(), "memory.read");
        assert_eq!(AuditAction::MemorySearch.as_str(), "memory.search");
        assert_eq!(AuditAction::MemoryContext.as_str(), "memory.context");
        assert_eq!(
            AuditAction::MemoryMaintenance.as_str(),
            "memory.maintenance"
        );
        assert_eq!(
            AuditAction::HardwareDriverStarted.as_str(),
            "hardware.driver.started"
        );
        assert_eq!(
            AuditAction::HardwareDriverStopped.as_str(),
            "hardware.driver.stopped"
        );
        assert_eq!(
            AuditAction::HardwareHotplugPublished.as_str(),
            "hardware.hotplug.published"
        );
        assert_eq!(AuditOutcome::Blocked.as_str(), "blocked");
        assert_eq!(
            AuditAction::from_stable_name("provider.supervised"),
            Some(AuditAction::ProviderSupervised)
        );
    }

    #[test]
    /// 中文：验证内存写入端保留消息、追踪和扩展字段。
    fn in_memory_sink_records_events() {
        let trace = TraceFields::default().with_span_id(SpanId::parse("span-1").unwrap());
        let event = AuditEvent::new(AuditAction::PolicyEvaluated, AuditOutcome::Ok, trace)
            .with_message("policy ok")
            .with_field("layer_count", "3");
        let mut sink = InMemoryAuditSink::default();

        sink.record(event).unwrap();

        assert_eq!(sink.events.len(), 1);
        assert_eq!(sink.events[0].message.as_deref(), Some("policy ok"));
        assert_eq!(sink.events[0].fields[0], ("layer_count".into(), "3".into()));
    }
}
