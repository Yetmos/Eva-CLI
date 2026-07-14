//! 维护 MCP 会话、逻辑流和孤儿记录的内存生命周期。
//!
//! 注册表的变更均要求可变独占访问；关闭成功后才移除会话，运行中的逻辑流会随会话关闭
//! 标记为中止。孤儿清理只删除已停止或外部检查确认进程不存在的登记项，不负责终止进程。
//! MCP session registry and lifecycle supervisor boundary.

use crate::session::{
    McpServerTransport, McpSession, McpSessionConfig, McpSessionManager, McpSessionSupervisor,
};
use eva_core::{AdapterId, EvaError};
use std::collections::{BTreeMap, BTreeSet};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP session registry, health, stream abort, and orphan cleanup";

/// 定义 `McpSessionLifecycleStatus` 可取的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpSessionLifecycleStatus {
    /// 表示 `Running` 枚举分支。
    Running,
    /// 表示 `Stopped` 枚举分支。
    Stopped,
    /// 表示 `Orphaned` 枚举分支。
    Orphaned,
}

/// 定义 `McpStreamStatus` 可取的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpStreamStatus {
    /// 表示 `Running` 枚举分支。
    Running,
    /// 表示 `Aborted` 枚举分支。
    Aborted,
}

/// 表示 `McpSessionLifecycleReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSessionLifecycleReport {
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `status` 字段对应的值。
    pub status: McpSessionLifecycleStatus,
    /// 记录 `process_id` 字段对应的值。
    pub process_id: Option<u32>,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

/// 表示 `McpSessionHealthReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSessionHealthReport {
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `status` 字段对应的值。
    pub status: McpSessionLifecycleStatus,
    /// 记录 `healthy` 字段对应的值。
    pub healthy: bool,
    /// 记录 `active_streams` 字段对应的值。
    pub active_streams: Vec<String>,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

/// 表示 `McpRegisteredSession` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpRegisteredSession {
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `process_id` 字段对应的值。
    pub process_id: Option<u32>,
    /// 记录 `server_transport` 字段对应的值。
    pub server_transport: McpServerTransport,
    /// 记录 `explicit_tools` 字段对应的值。
    pub explicit_tools: Vec<String>,
    /// 记录 `active_streams` 字段对应的值。
    pub active_streams: Vec<String>,
}

/// 表示 `McpStreamReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStreamReport {
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `stream_id` 字段对应的值。
    pub stream_id: String,
    /// 记录 `status` 字段对应的值。
    pub status: McpStreamStatus,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

/// 表示 `McpOrphanCleanupReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpOrphanCleanupReport {
    /// 记录 `removed_sessions` 字段对应的值。
    pub removed_sessions: Vec<String>,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

/// 约定 `McpProcessInspector` 实现需要满足的接口。
pub trait McpProcessInspector {
    /// 执行 `process_is_running` 对应的处理逻辑。
    fn process_is_running(&self, process_id: u32) -> bool;
}

/// 表示 `McpSessionRegistry` 数据结构。
#[derive(Debug, Default)]
pub struct McpSessionRegistry {
    /// 记录 `sessions` 字段对应的值。
    sessions: BTreeMap<String, RegisteredSession>,
}

/// 表示 `RegisteredSession` 数据结构。
#[derive(Debug)]
struct RegisteredSession {
    /// 记录 `session` 字段对应的值。
    session: McpSession,
    /// 记录 `process_id` 字段对应的值。
    process_id: Option<u32>,
    /// 记录 `server_transport` 字段对应的值。
    server_transport: McpServerTransport,
    /// 记录 `explicit_tools` 字段对应的值。
    explicit_tools: BTreeSet<String>,
    /// 记录 `streams` 字段对应的值。
    streams: BTreeMap<String, McpStreamStatus>,
}

impl McpSessionLifecycleStatus {
    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Orphaned => "orphaned",
        }
    }
}

impl McpStreamStatus {
    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Aborted => "aborted",
        }
    }
}

impl McpSessionRegistry {
    /// 创建并初始化当前类型的实例。
    pub fn new() -> Self {
        Self::default()
    }

    /// 执行 `start` 对应的受控流程。
    pub fn start(
        &mut self,
        supervisor: &mut impl McpSessionSupervisor,
        config: McpSessionConfig,
        explicit_tools: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<McpSessionLifecycleReport, EvaError> {
        let explicit_tools = validate_explicit_tools(explicit_tools)?;
        let manager = McpSessionManager;
        let session = manager.start(supervisor, config)?;
        let start_report = session.start_report()?;
        let session_id = start_report.session_id.clone();
        if self.sessions.contains_key(&session_id) {
            return Err(EvaError::conflict("MCP session already exists")
                .with_context("session_id", session_id));
        }
        let process_id = session.process_id();
        let server_transport = session.server_transport();
        self.sessions.insert(
            session_id.clone(),
            RegisteredSession {
                session,
                process_id,
                server_transport,
                explicit_tools,
                streams: BTreeMap::new(),
            },
        );

        let mut audit = start_report.audit;
        audit.push("mcp.session_registry:registered".to_owned());
        Ok(McpSessionLifecycleReport {
            adapter_id: start_report.adapter_id,
            session_id,
            status: McpSessionLifecycleStatus::Running,
            process_id,
            audit,
        })
    }

    /// 返回 `list` 对应的数据视图。
    pub fn list(&self) -> Vec<McpRegisteredSession> {
        self.sessions
            .iter()
            .map(|(session_id, entry)| McpRegisteredSession {
                adapter_id: entry.session.adapter_id().clone(),
                session_id: session_id.clone(),
                process_id: entry.process_id,
                server_transport: entry.server_transport,
                explicit_tools: entry.explicit_tools.iter().cloned().collect(),
                active_streams: active_streams(entry),
            })
            .collect()
    }

    /// 执行 `health` 对应的处理逻辑。
    pub fn health(&self, session_id: &str) -> Result<McpSessionHealthReport, EvaError> {
        let entry = self.session(session_id)?;
        let healthy = entry.session.is_running();
        Ok(McpSessionHealthReport {
            adapter_id: entry.session.adapter_id().clone(),
            session_id: session_id.to_owned(),
            status: if healthy {
                McpSessionLifecycleStatus::Running
            } else {
                McpSessionLifecycleStatus::Stopped
            },
            healthy,
            active_streams: active_streams(entry),
            audit: vec![
                "transport:mcp".to_owned(),
                "mcp.session:health_checked".to_owned(),
                format!("healthy:{healthy}"),
            ],
        })
    }

    /// 停止或释放 `shutdown` 管理的资源。
    pub fn shutdown(
        &mut self,
        supervisor: &mut impl McpSessionSupervisor,
        session_id: &str,
    ) -> Result<McpSessionLifecycleReport, EvaError> {
        let entry = self.session_mut(session_id)?;
        let process_id = entry.process_id;
        let manager = McpSessionManager;
        let shutdown_report = manager.shutdown(supervisor, &mut entry.session)?;
        let removed = self.sessions.remove(session_id).ok_or_else(|| {
            EvaError::internal("MCP session registry lost session during shutdown")
                .with_context("session_id", session_id)
        })?;
        let mut audit = shutdown_report.audit;
        if removed
            .streams
            .values()
            .any(|status| *status == McpStreamStatus::Running)
        {
            audit.push("mcp.stream:aborted_by_session_shutdown".to_owned());
        }
        audit.push("mcp.session_registry:removed".to_owned());

        Ok(McpSessionLifecycleReport {
            adapter_id: shutdown_report.adapter_id,
            session_id: shutdown_report.session_id,
            status: McpSessionLifecycleStatus::Stopped,
            process_id,
            audit,
        })
    }

    /// 执行 `start_stream` 对应的受控流程。
    pub fn start_stream(
        &mut self,
        session_id: &str,
        stream_id: impl Into<String>,
    ) -> Result<McpStreamReport, EvaError> {
        let stream_id = validate_stream_id(stream_id.into())?;
        let entry = self.session_mut(session_id)?;
        if matches!(
            entry.streams.get(&stream_id),
            Some(McpStreamStatus::Running)
        ) {
            return Err(EvaError::conflict("MCP stream is already running")
                .with_context("session_id", session_id)
                .with_context("stream_id", &stream_id));
        }
        entry
            .streams
            .insert(stream_id.clone(), McpStreamStatus::Running);
        Ok(McpStreamReport {
            adapter_id: entry.session.adapter_id().clone(),
            session_id: session_id.to_owned(),
            stream_id,
            status: McpStreamStatus::Running,
            audit: vec![
                "transport:mcp".to_owned(),
                "mcp.stream:started".to_owned(),
                "stream_boundary:controlled".to_owned(),
            ],
        })
    }

    /// 执行 `abort_stream` 对应的处理逻辑。
    pub fn abort_stream(
        &mut self,
        session_id: &str,
        stream_id: &str,
    ) -> Result<McpStreamReport, EvaError> {
        let entry = self.session_mut(session_id)?;
        let status = entry.streams.get_mut(stream_id).ok_or_else(|| {
            EvaError::not_found("MCP stream does not exist")
                .with_context("session_id", session_id)
                .with_context("stream_id", stream_id)
        })?;
        *status = McpStreamStatus::Aborted;
        Ok(McpStreamReport {
            adapter_id: entry.session.adapter_id().clone(),
            session_id: session_id.to_owned(),
            stream_id: stream_id.to_owned(),
            status: McpStreamStatus::Aborted,
            audit: vec![
                "transport:mcp".to_owned(),
                "mcp.stream:abort_requested".to_owned(),
                "mcp.stream:aborted".to_owned(),
            ],
        })
    }

    /// 执行 `cleanup_orphans` 对应的处理逻辑。
    pub fn cleanup_orphans(
        &mut self,
        inspector: &impl McpProcessInspector,
    ) -> McpOrphanCleanupReport {
        let orphaned: Vec<String> = self
            .sessions
            .iter()
            .filter_map(|(session_id, entry)| {
                let process_missing = entry
                    .process_id
                    .is_some_and(|process_id| !inspector.process_is_running(process_id));
                if !entry.session.is_running() || process_missing {
                    Some(session_id.clone())
                } else {
                    None
                }
            })
            .collect();

        for session_id in &orphaned {
            self.sessions.remove(session_id);
        }

        let mut audit = vec![
            "transport:mcp".to_owned(),
            "mcp.session:orphan_cleanup_checked".to_owned(),
            format!("removed_count:{}", orphaned.len()),
        ];
        audit.extend(
            orphaned
                .iter()
                .map(|session_id| format!("mcp.session:orphan_removed:{session_id}")),
        );

        McpOrphanCleanupReport {
            removed_sessions: orphaned,
            audit,
        }
    }

    /// 执行 `session` 对应的处理逻辑。
    fn session(&self, session_id: &str) -> Result<&RegisteredSession, EvaError> {
        self.sessions.get(session_id).ok_or_else(|| {
            EvaError::not_found("MCP session is not registered")
                .with_context("session_id", session_id)
        })
    }

    /// 执行 `session_mut` 对应的处理逻辑。
    fn session_mut(&mut self, session_id: &str) -> Result<&mut RegisteredSession, EvaError> {
        self.sessions.get_mut(session_id).ok_or_else(|| {
            EvaError::not_found("MCP session is not registered")
                .with_context("session_id", session_id)
        })
    }
}

/// 执行 `active_streams` 对应的处理逻辑。
fn active_streams(entry: &RegisteredSession) -> Vec<String> {
    entry
        .streams
        .iter()
        .filter_map(|(stream_id, status)| {
            if *status == McpStreamStatus::Running {
                Some(stream_id.clone())
            } else {
                None
            }
        })
        .collect()
}

/// 校验 `validate_explicit_tools` 对应的约束，不满足时返回明确错误。
fn validate_explicit_tools(
    tools: impl IntoIterator<Item = impl Into<String>>,
) -> Result<BTreeSet<String>, EvaError> {
    let mut explicit = BTreeSet::new();
    for tool in tools {
        let tool = tool.into();
        if tool.is_empty() || tool.trim() != tool || tool.chars().any(char::is_whitespace) {
            return Err(EvaError::invalid_argument(
                "MCP explicit tool name must be non-empty and stable",
            )
            .with_context("tool", tool));
        }
        explicit.insert(tool);
    }
    Ok(explicit)
}

/// 校验 `validate_stream_id` 对应的约束，不满足时返回明确错误。
fn validate_stream_id(stream_id: String) -> Result<String, EvaError> {
    if stream_id.is_empty()
        || stream_id.trim() != stream_id
        || stream_id.chars().any(char::is_whitespace)
    {
        return Err(EvaError::invalid_argument("MCP stream id must be stable")
            .with_context("stream_id", stream_id));
    }
    Ok(stream_id)
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{
        McpProcessHandle, McpProcessShutdownRequest, McpProcessSpec, McpProcessStartRequest,
    };
    use eva_core::ErrorKind;

    /// 表示 `FakeSupervisor` 数据结构。
    #[derive(Debug, Clone)]
    struct FakeSupervisor {
        /// 记录 `next_session_id` 字段对应的值。
        next_session_id: String,
        /// 记录 `process_id` 字段对应的值。
        process_id: Option<u32>,
        /// 记录 `shutdown_calls` 字段对应的值。
        shutdown_calls: Vec<String>,
    }

    impl FakeSupervisor {
        /// 创建并初始化当前类型的实例。
        fn new(session_id: &str, process_id: Option<u32>) -> Self {
            Self {
                next_session_id: session_id.to_owned(),
                process_id,
                shutdown_calls: Vec::new(),
            }
        }
    }

    impl McpSessionSupervisor for FakeSupervisor {
        /// 执行 `start_process` 对应的受控流程。
        fn start_process(
            &mut self,
            request: &McpProcessStartRequest,
        ) -> Result<McpProcessHandle, EvaError> {
            assert_eq!(request.command, "github-mcp-server");
            Ok(McpProcessHandle::new(
                self.next_session_id.clone(),
                self.process_id,
            ))
        }

        /// 停止或释放 `shutdown_process` 管理的资源。
        fn shutdown_process(
            &mut self,
            request: &McpProcessShutdownRequest,
        ) -> Result<(), EvaError> {
            self.shutdown_calls.push(request.session_id.clone());
            Ok(())
        }
    }

    /// 表示 `StaticInspector` 数据结构。
    #[derive(Debug, Clone, Copy)]
    struct StaticInspector {
        /// 记录 `running` 字段对应的值。
        running: bool,
    }

    impl McpProcessInspector for StaticInspector {
        /// 执行 `process_is_running` 对应的处理逻辑。
        fn process_is_running(&self, _process_id: u32) -> bool {
            self.running
        }
    }

    /// 验证 `registry_start_health_shutdown_removes_session` 场景下的预期行为。
    #[test]
    fn registry_start_health_shutdown_removes_session() {
        let mut registry = McpSessionRegistry::new();
        let mut supervisor = FakeSupervisor::new("session-1", Some(42));

        let start = registry
            .start(&mut supervisor, config(), ["list_issues"])
            .unwrap();
        let health = registry.health(&start.session_id).unwrap();

        assert_eq!(start.status, McpSessionLifecycleStatus::Running);
        assert!(health.healthy);
        assert_eq!(registry.list().len(), 1);

        let stop = registry
            .shutdown(&mut supervisor, &start.session_id)
            .unwrap();

        assert_eq!(stop.status, McpSessionLifecycleStatus::Stopped);
        assert_eq!(supervisor.shutdown_calls, vec!["session-1".to_owned()]);
        assert!(registry.list().is_empty());
    }

    /// 验证 `stream_can_be_aborted_and_removed_from_health` 场景下的预期行为。
    #[test]
    fn stream_can_be_aborted_and_removed_from_health() {
        let mut registry = McpSessionRegistry::new();
        let mut supervisor = FakeSupervisor::new("session-2", Some(7));
        let start = registry
            .start(&mut supervisor, config(), ["list_issues"])
            .unwrap();

        let stream = registry
            .start_stream(&start.session_id, "stream-1")
            .unwrap();
        assert_eq!(stream.status, McpStreamStatus::Running);
        assert_eq!(
            registry.health(&start.session_id).unwrap().active_streams,
            vec!["stream-1".to_owned()]
        );

        let aborted = registry
            .abort_stream(&start.session_id, "stream-1")
            .unwrap();

        assert_eq!(aborted.status, McpStreamStatus::Aborted);
        assert!(registry
            .health(&start.session_id)
            .unwrap()
            .active_streams
            .is_empty());
    }

    /// 验证 `orphan_cleanup_removes_missing_process_session` 场景下的预期行为。
    #[test]
    fn orphan_cleanup_removes_missing_process_session() {
        let mut registry = McpSessionRegistry::new();
        let mut supervisor = FakeSupervisor::new("session-3", Some(99));
        registry
            .start(&mut supervisor, config(), ["list_issues"])
            .unwrap();

        let cleanup = registry.cleanup_orphans(&StaticInspector { running: false });

        assert_eq!(cleanup.removed_sessions, vec!["session-3".to_owned()]);
        assert!(registry.list().is_empty());
        assert!(cleanup
            .audit
            .contains(&"mcp.session:orphan_cleanup_checked".to_owned()));
    }

    /// 验证 `invalid_stream_id_is_rejected` 场景下的预期行为。
    #[test]
    fn invalid_stream_id_is_rejected() {
        let mut registry = McpSessionRegistry::new();
        let mut supervisor = FakeSupervisor::new("session-4", Some(9));
        let start = registry
            .start(&mut supervisor, config(), ["list_issues"])
            .unwrap();

        let error = registry
            .start_stream(&start.session_id, "bad stream")
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

    /// 执行 `config` 对应的处理逻辑。
    fn config() -> McpSessionConfig {
        let process = McpProcessSpec::new("github-mcp-server");
        McpSessionConfig::new(
            AdapterId::parse("github-mcp").unwrap(),
            McpServerTransport::Stdio,
            process,
        )
        .unwrap()
    }
}
