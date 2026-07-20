//! 维护 MCP 会话、逻辑流和孤儿记录的内存生命周期。
//!
//! 注册表的变更均要求可变独占访问；关闭成功后才移除会话，运行中的逻辑流会随会话关闭
//! 标记为中止。孤儿清理只删除已停止或外部检查确认进程不存在的登记项，不负责终止进程。
//! MCP session registry and lifecycle supervisor boundary.

use crate::session::{
    McpServerTransport, McpSession, McpSessionConfig, McpSessionManager, McpSessionSupervisor,
};
use crate::sse::{retained_item_bytes, McpSseAbortHandle, McpSseEventStream, McpSseItem};
use crate::streamable_http::McpStreamableHttpSession;
use eva_core::{AdapterId, EvaError};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP session registry, health, stream abort, and orphan cleanup";

const MANAGED_STREAM_QUEUE_ITEMS: usize = 64;

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

/// Registry-owned Streamable HTTP sessions and their real reader threads.
///
/// This registry is deliberately separate from the stdio process registry:
/// W4-L06 owns deterministic socket/session cleanup, while W4-L07 will decide
/// which daemon supervisor owns this registry for the long term.
pub struct McpStreamableHttpSessionRegistry {
    sessions: BTreeMap<String, RegisteredHttpSession>,
    next_session_id: u64,
}

struct RegisteredHttpSession {
    adapter_id: AdapterId,
    session: McpStreamableHttpSession,
    state: ManagedHttpSessionState,
    streams: BTreeMap<String, RegisteredHttpStream>,
}

struct RegisteredHttpStream {
    state: ManagedHttpStreamState,
    cancellation: Arc<AtomicBool>,
    abort: McpSseAbortHandle,
    events: Receiver<McpSseItem>,
    queued_bytes: Arc<AtomicUsize>,
    done: Receiver<ManagedReaderExit>,
    join: Option<JoinHandle<()>>,
    last_exit: Option<ManagedReaderExit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedHttpStreamState {
    Running,
    Cancelling,
    Joined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedHttpSessionState {
    Active,
    CleanupPending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ManagedReaderExit {
    Cancelled,
    SourceEnded { provider_code: Option<String> },
    QueueLimit,
}

impl ManagedReaderExit {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::SourceEnded { .. } => "source_ended",
            Self::QueueLimit => "queue_limit",
        }
    }
}

impl Default for McpStreamableHttpSessionRegistry {
    fn default() -> Self {
        Self {
            sessions: BTreeMap::new(),
            next_session_id: 1,
        }
    }
}

impl fmt::Debug for McpStreamableHttpSessionRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpStreamableHttpSessionRegistry")
            .field("session_count", &self.sessions.len())
            .field("dangling_reader_count", &self.dangling_reader_count())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for RegisteredHttpSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegisteredHttpSession")
            .field("adapter_id", &self.adapter_id)
            .field("session", &self.session)
            .field("state", &self.state)
            .field("stream_count", &self.streams.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for RegisteredHttpStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegisteredHttpStream")
            .field("state", &self.state)
            .field("queued_bytes", &self.queued_bytes.load(Ordering::Acquire))
            .field("reader_join_pending", &self.join.is_some())
            .field(
                "reader_finished",
                &self.join.as_ref().is_none_or(JoinHandle::is_finished),
            )
            .field("last_exit", &self.last_exit)
            .finish_non_exhaustive()
    }
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

impl McpStreamableHttpSessionRegistry {
    /// Create an empty registry. Registry IDs are local control identifiers;
    /// opaque server-issued session IDs never leave the owned session.
    pub fn new() -> Self {
        Self::default()
    }

    /// Transfer one Streamable HTTP session to the registry. A session that
    /// is not ready is retained in cleanup-only state instead of losing its
    /// opaque DELETE material on a best-effort cleanup failure.
    pub fn register_session(
        &mut self,
        adapter_id: AdapterId,
        session: McpStreamableHttpSession,
    ) -> Result<McpSessionLifecycleReport, EvaError> {
        let ready = session.is_ready();
        let next_session_id = self.next_session_id.checked_add(1).ok_or_else(|| {
            EvaError::conflict("MCP Streamable HTTP registry ID space is exhausted")
                .with_provider_code("mcp_http_registry_id_exhausted")
        })?;
        let registry_session_id = format!("mcp-http-session-{}", self.next_session_id);
        self.next_session_id = next_session_id;
        self.sessions.insert(
            registry_session_id.clone(),
            RegisteredHttpSession {
                adapter_id: adapter_id.clone(),
                session,
                state: if ready {
                    ManagedHttpSessionState::Active
                } else {
                    ManagedHttpSessionState::CleanupPending
                },
                streams: BTreeMap::new(),
            },
        );
        let mut audit = vec![
            "transport:mcp_streamable_http".to_owned(),
            "mcp.http_registry:session_registered".to_owned(),
            "mcp.http_registry:opaque_session_redacted".to_owned(),
        ];
        if !ready {
            audit.push("mcp.http_registry:session_requires_cleanup".to_owned());
        }
        Ok(McpSessionLifecycleReport {
            adapter_id,
            session_id: registry_session_id,
            status: if ready {
                McpSessionLifecycleStatus::Running
            } else {
                McpSessionLifecycleStatus::Orphaned
            },
            process_id: None,
            audit,
        })
    }

    /// Open a GET event stream and transfer its real reader to a bounded
    /// registry-owned thread.
    pub fn open_event_stream(
        &mut self,
        session_id: &str,
        stream_id: impl Into<String>,
    ) -> Result<McpStreamReport, EvaError> {
        let stream_id = validate_stream_id(stream_id.into())?;
        let entry = self.http_session_mut(session_id)?;
        ensure_http_session_active(entry)?;
        ensure_http_stream_absent(entry, session_id, &stream_id)?;
        let event_stream = entry.session.open_event_stream()?;
        let registered = spawn_managed_http_reader(event_stream)?;
        entry.streams.insert(stream_id.clone(), registered);
        Ok(http_stream_report(
            &entry.adapter_id,
            session_id,
            stream_id,
            McpStreamStatus::Running,
            vec![
                "mcp.stream:reader_owned_by_registry".to_owned(),
                "mcp.stream:socket_abort_handle_registered".to_owned(),
            ],
        ))
    }

    /// Open a POST event stream and transfer its real reader to a bounded
    /// registry-owned thread.
    pub fn post_event_stream(
        &mut self,
        session_id: &str,
        stream_id: impl Into<String>,
        request_id: u64,
        request: &str,
    ) -> Result<McpStreamReport, EvaError> {
        let stream_id = validate_stream_id(stream_id.into())?;
        let entry = self.http_session_mut(session_id)?;
        ensure_http_session_active(entry)?;
        ensure_http_stream_absent(entry, session_id, &stream_id)?;
        let event_stream = entry.session.post_event_stream(request_id, request)?;
        let registered = spawn_managed_http_reader(event_stream)?;
        entry.streams.insert(stream_id.clone(), registered);
        Ok(http_stream_report(
            &entry.adapter_id,
            session_id,
            stream_id,
            McpStreamStatus::Running,
            vec![
                "mcp.stream:reader_owned_by_registry".to_owned(),
                "mcp.stream:socket_abort_handle_registered".to_owned(),
            ],
        ))
    }

    /// Non-blockingly take the next decoded event from the bounded reader
    /// queue. `None` means no event is currently available.
    pub fn try_next_stream_item(
        &mut self,
        session_id: &str,
        stream_id: &str,
    ) -> Result<Option<McpSseItem>, EvaError> {
        let entry = self.http_session_mut(session_id)?;
        ensure_http_session_active(entry)?;
        let stream = entry.streams.get_mut(stream_id).ok_or_else(|| {
            EvaError::not_found("MCP Streamable HTTP stream is not registered")
                .with_provider_code("mcp_http_registry_stream_not_found")
        })?;
        match stream.events.try_recv() {
            Ok(item) => {
                release_queued_bytes(&stream.queued_bytes, retained_item_bytes(&item));
                Ok(Some(item))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                let timeout = entry.session.lifecycle_timeout();
                let exit = wait_and_join_reader(stream, timeout)?;
                let error = managed_reader_terminal_error(stream.last_exit.as_ref(), exit);
                entry.streams.remove(stream_id).ok_or_else(|| {
                    EvaError::internal(
                        "MCP Streamable HTTP stream disappeared while reaping its reader",
                    )
                    .with_provider_code("mcp_http_registry_stream_lost")
                })?;
                Err(error)
            }
        }
    }

    /// Abort one real reader. If this is the final stream, the owned MCP
    /// application session is DELETEd and removed as one cleanup transaction.
    pub fn abort_stream(
        &mut self,
        session_id: &str,
        stream_id: &str,
    ) -> Result<McpStreamReport, EvaError> {
        let (adapter_id, final_stream, timeout, cancellation, abort) = {
            let entry = self.http_session_mut(session_id)?;
            if !entry.streams.contains_key(stream_id) {
                return Err(
                    EvaError::not_found("MCP Streamable HTTP stream is not registered")
                        .with_provider_code("mcp_http_registry_stream_not_found"),
                );
            }
            let final_stream = entry.streams.len() == 1;
            if final_stream {
                entry.state = ManagedHttpSessionState::CleanupPending;
            }
            let stream = entry
                .streams
                .get_mut(stream_id)
                .expect("stream existence was checked above");
            stream.state = ManagedHttpStreamState::Cancelling;
            (
                entry.adapter_id.clone(),
                final_stream,
                entry.session.lifecycle_timeout(),
                stream.cancellation.clone(),
                stream.abort.clone(),
            )
        };

        cancellation.store(true, Ordering::Release);
        let socket_error = abort.abort().err();
        if let Some(error) = socket_error {
            let delete_outcome = if final_stream {
                "not_attempted"
            } else {
                "not_required"
            };
            return Err(error
                .with_context("socket_closed", "false")
                .with_context("session_delete_outcome", delete_outcome)
                .with_context("reader_joined", "false")
                .with_context("cleanup_pending", "true"));
        }
        let delete_error = if final_stream {
            self.http_session_mut(session_id)?.session.shutdown().err()
        } else {
            None
        };
        let reader_result = {
            let entry = self.http_session_mut(session_id)?;
            let stream = entry.streams.get_mut(stream_id).ok_or_else(|| {
                EvaError::internal("MCP Streamable HTTP stream disappeared during abort")
                    .with_provider_code("mcp_http_registry_stream_lost")
            })?;
            wait_and_join_reader(stream, timeout)
        };
        let (reader_exit, reader_error) = match reader_result {
            Ok(exit) => (exit, None),
            Err(error) => ("not_joined", Some(error)),
        };

        let delete_outcome = if !final_stream {
            "not_required"
        } else if delete_error.is_none() {
            "complete"
        } else {
            "failed"
        };
        let reader_joined = reader_error.is_none();
        let mut failure = delete_error;
        if failure.is_none() {
            failure = reader_error;
        }
        if let Some(error) = failure {
            return Err(error
                .with_context("socket_closed", "true")
                .with_context("session_delete_outcome", delete_outcome)
                .with_context("reader_joined", reader_joined.to_string())
                .with_context("cleanup_pending", "true"));
        }

        if final_stream {
            self.sessions.remove(session_id).ok_or_else(|| {
                EvaError::internal("MCP Streamable HTTP session disappeared during cleanup")
                    .with_provider_code("mcp_http_registry_session_lost")
            })?;
        } else {
            self.http_session_mut(session_id)?
                .streams
                .remove(stream_id)
                .ok_or_else(|| {
                    EvaError::internal("MCP Streamable HTTP stream disappeared during cleanup")
                        .with_provider_code("mcp_http_registry_stream_lost")
                })?;
        }
        let mut audit = vec![
            "mcp.stream:abort_requested".to_owned(),
            "mcp.stream:socket_shutdown".to_owned(),
            format!("mcp.stream:reader_joined:{reader_exit}"),
        ];
        if final_stream {
            audit.extend([
                "mcp.http:session_delete_complete".to_owned(),
                "mcp.http_registry:session_removed".to_owned(),
                "mcp.http_registry:session_dangling_readers:0".to_owned(),
            ]);
        } else {
            audit.push("mcp.http_registry:session_retained_for_other_streams".to_owned());
        }
        Ok(http_stream_report(
            &adapter_id,
            session_id,
            stream_id.to_owned(),
            McpStreamStatus::Aborted,
            audit,
        ))
    }

    /// Cancel every reader, DELETE the application session, join all reader
    /// threads, and remove registry state only after the transaction completes.
    pub fn shutdown_session(
        &mut self,
        session_id: &str,
    ) -> Result<McpSessionLifecycleReport, EvaError> {
        let (adapter_id, timeout, stream_ids, controls) = {
            let entry = self.http_session_mut(session_id)?;
            entry.state = ManagedHttpSessionState::CleanupPending;
            let timeout = entry.session.lifecycle_timeout();
            let stream_ids = entry.streams.keys().cloned().collect::<Vec<_>>();
            let controls = entry
                .streams
                .values_mut()
                .map(|stream| {
                    stream.state = ManagedHttpStreamState::Cancelling;
                    stream.cancellation.store(true, Ordering::Release);
                    stream.abort.clone()
                })
                .collect::<Vec<_>>();
            (entry.adapter_id.clone(), timeout, stream_ids, controls)
        };

        let mut failure = None;
        let mut sockets_closed = true;
        for abort in controls {
            if let Err(error) = abort.abort() {
                sockets_closed = false;
                if failure.is_none() {
                    failure = Some(error);
                }
            }
        }
        if !sockets_closed {
            let error = failure.expect("a failed socket shutdown records its error");
            return Err(error
                .with_context("sockets_closed", "false")
                .with_context("session_delete_complete", "false")
                .with_context("readers_joined", "false")
                .with_context("cleanup_pending", "true"));
        }
        let delete_error = self.http_session_mut(session_id)?.session.shutdown().err();
        let session_deleted = delete_error.is_none();
        if failure.is_none() {
            failure = delete_error;
        }

        let mut readers_joined = true;
        for stream_id in &stream_ids {
            let result = {
                let entry = self.http_session_mut(session_id)?;
                let stream = entry.streams.get_mut(stream_id).ok_or_else(|| {
                    EvaError::internal("MCP Streamable HTTP stream disappeared during shutdown")
                        .with_provider_code("mcp_http_registry_stream_lost")
                })?;
                wait_and_join_reader(stream, timeout)
            };
            if let Err(error) = result {
                readers_joined = false;
                if failure.is_none() {
                    failure = Some(error);
                }
            }
        }
        if let Some(error) = failure {
            return Err(error
                .with_context("sockets_closed", sockets_closed.to_string())
                .with_context("session_delete_complete", session_deleted.to_string())
                .with_context("readers_joined", readers_joined.to_string())
                .with_context("cleanup_pending", "true"));
        }

        self.sessions.remove(session_id).ok_or_else(|| {
            EvaError::internal("MCP Streamable HTTP session disappeared during shutdown")
                .with_provider_code("mcp_http_registry_session_lost")
        })?;
        Ok(McpSessionLifecycleReport {
            adapter_id,
            session_id: session_id.to_owned(),
            status: McpSessionLifecycleStatus::Stopped,
            process_id: None,
            audit: vec![
                "transport:mcp_streamable_http".to_owned(),
                "mcp.stream:all_sockets_shutdown".to_owned(),
                "mcp.http:session_delete_complete".to_owned(),
                "mcp.stream:all_readers_joined".to_owned(),
                "mcp.http_registry:session_removed".to_owned(),
                "mcp.http_registry:session_dangling_readers:0".to_owned(),
            ],
        })
    }

    /// Number of application sessions that still require explicit cleanup.
    pub fn dangling_session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Number of reader threads that have not yet been joined and reaped.
    pub fn dangling_reader_count(&self) -> usize {
        self.sessions
            .values()
            .map(|entry| {
                entry
                    .streams
                    .values()
                    .filter(|stream| stream.join.is_some())
                    .count()
            })
            .sum()
    }

    fn http_session_mut(
        &mut self,
        session_id: &str,
    ) -> Result<&mut RegisteredHttpSession, EvaError> {
        self.sessions.get_mut(session_id).ok_or_else(|| {
            EvaError::not_found("MCP Streamable HTTP session is not registered")
                .with_provider_code("mcp_http_registry_session_not_found")
        })
    }
}

impl Drop for McpStreamableHttpSessionRegistry {
    fn drop(&mut self) {
        for entry in self.sessions.values_mut() {
            let timeout = entry.session.lifecycle_timeout();
            let mut sockets_closed = true;
            for stream in entry.streams.values_mut() {
                stream.cancellation.store(true, Ordering::Release);
                if stream.abort.abort().is_err() {
                    sockets_closed = false;
                }
            }
            if sockets_closed {
                let _ = entry.session.shutdown();
            }
            for stream in entry.streams.values_mut() {
                let _ = wait_and_join_reader(stream, timeout);
            }
            if !sockets_closed {
                sockets_closed = true;
                for stream in entry.streams.values() {
                    if stream.abort.abort().is_err() {
                        sockets_closed = false;
                    }
                }
                if sockets_closed {
                    let _ = entry.session.shutdown();
                    for stream in entry.streams.values_mut() {
                        let _ = wait_and_join_reader(stream, timeout);
                    }
                }
            }
        }
    }
}

fn ensure_http_stream_absent(
    entry: &RegisteredHttpSession,
    session_id: &str,
    stream_id: &str,
) -> Result<(), EvaError> {
    if entry.streams.contains_key(stream_id) {
        return Err(
            EvaError::conflict("MCP Streamable HTTP stream already exists")
                .with_provider_code("mcp_http_registry_stream_exists")
                .with_context("session_id", session_id)
                .with_context("stream_id", stream_id),
        );
    }
    Ok(())
}

fn ensure_http_session_active(entry: &RegisteredHttpSession) -> Result<(), EvaError> {
    if entry.state == ManagedHttpSessionState::CleanupPending {
        return Err(
            EvaError::conflict("MCP Streamable HTTP session is fenced for cleanup")
                .with_provider_code("mcp_http_registry_cleanup_pending"),
        );
    }
    Ok(())
}

fn spawn_managed_http_reader(stream: McpSseEventStream) -> Result<RegisteredHttpStream, EvaError> {
    let abort = stream.abort_handle()?;
    let output_limit_bytes = stream.output_limit_bytes();
    let cancellation = Arc::new(AtomicBool::new(false));
    let queued_bytes = Arc::new(AtomicUsize::new(0));
    let (event_sender, events) = mpsc::sync_channel(MANAGED_STREAM_QUEUE_ITEMS);
    let (done_sender, done) = mpsc::sync_channel(1);
    let reader_cancellation = cancellation.clone();
    let reader_queued_bytes = queued_bytes.clone();
    let cleanup_abort = abort.clone();
    let reader_abort = abort.clone();
    let join = thread::Builder::new()
        .name("eva-mcp-sse-reader".to_owned())
        .spawn(move || {
            let exit = run_managed_http_reader(
                stream,
                &reader_cancellation,
                &reader_queued_bytes,
                output_limit_bytes,
                &event_sender,
            );
            let _ = reader_abort.abort();
            let _ = done_sender.try_send(exit);
        })
        .map_err(|error| {
            cancellation.store(true, Ordering::Release);
            let _ = cleanup_abort.abort();
            EvaError::unavailable("failed to start MCP SSE reader thread")
                .with_provider_code("mcp_stream_reader_spawn_failed")
                .with_context("io_error_kind", format!("{:?}", error.kind()))
        })?;
    Ok(RegisteredHttpStream {
        state: ManagedHttpStreamState::Running,
        cancellation,
        abort,
        events,
        queued_bytes,
        done,
        join: Some(join),
        last_exit: None,
    })
}

fn run_managed_http_reader(
    mut stream: McpSseEventStream,
    cancellation: &AtomicBool,
    queued_bytes: &AtomicUsize,
    output_limit_bytes: usize,
    event_sender: &mpsc::SyncSender<McpSseItem>,
) -> ManagedReaderExit {
    loop {
        if cancellation.load(Ordering::Acquire) {
            return ManagedReaderExit::Cancelled;
        }
        let item = match stream.next_item() {
            Ok(item) => item,
            Err(error) => {
                if cancellation.load(Ordering::Acquire) {
                    return ManagedReaderExit::Cancelled;
                }
                return ManagedReaderExit::SourceEnded {
                    provider_code: error.provider_code().map(|code| code.as_str().to_owned()),
                };
            }
        };
        let item_bytes = retained_item_bytes(&item);
        if !reserve_queued_bytes(queued_bytes, item_bytes, output_limit_bytes) {
            return ManagedReaderExit::QueueLimit;
        }
        match event_sender.try_send(item) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                release_queued_bytes(queued_bytes, item_bytes);
                return ManagedReaderExit::QueueLimit;
            }
        }
    }
}

fn reserve_queued_bytes(counter: &AtomicUsize, amount: usize, limit: usize) -> bool {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let Some(next) = current.checked_add(amount) else {
            return false;
        };
        if next > limit {
            return false;
        }
        match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(actual) => current = actual,
        }
    }
}

fn release_queued_bytes(counter: &AtomicUsize, amount: usize) {
    let _ = counter.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        Some(current.saturating_sub(amount))
    });
}

fn managed_reader_terminal_error(
    exit: Option<&ManagedReaderExit>,
    fallback: &'static str,
) -> EvaError {
    match exit {
        Some(ManagedReaderExit::QueueLimit) => {
            EvaError::conflict("MCP managed SSE event queue reached its bounded limit")
                .with_provider_code("mcp_stream_reader_queue_limit")
        }
        Some(ManagedReaderExit::SourceEnded { provider_code }) => {
            let error = EvaError::unavailable("MCP managed SSE reader terminated")
                .with_provider_code("mcp_stream_reader_ended");
            if let Some(provider_code) = provider_code {
                error.with_context("source_provider_code", provider_code)
            } else {
                error
            }
        }
        Some(ManagedReaderExit::Cancelled) => {
            EvaError::unavailable("MCP managed SSE reader was cancelled")
                .with_provider_code("mcp_stream_reader_cancelled")
        }
        None => EvaError::unavailable("MCP managed SSE reader terminated without an exit report")
            .with_provider_code("mcp_stream_reader_ended")
            .with_context("reader_exit", fallback),
    }
}

fn wait_and_join_reader(
    stream: &mut RegisteredHttpStream,
    timeout: Duration,
) -> Result<&'static str, EvaError> {
    let Some(join) = stream.join.as_ref() else {
        return Ok(stream
            .last_exit
            .as_ref()
            .map_or("already_joined", ManagedReaderExit::as_str));
    };
    let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
        EvaError::invalid_argument("MCP SSE reader join timeout is out of range")
            .with_provider_code("mcp_stream_reader_timeout_invalid")
    })?;
    let exit = loop {
        if join.is_finished() {
            break stream.done.try_recv().ok();
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(
                EvaError::timeout("MCP SSE reader did not exit after socket abort")
                    .with_provider_code("mcp_stream_reader_join_timeout"),
            );
        }
        let wait = remaining.min(Duration::from_millis(50));
        match stream.done.recv_timeout(wait) {
            Ok(exit) => break Some(exit),
            Err(RecvTimeoutError::Disconnected) => break None,
            Err(RecvTimeoutError::Timeout) => stream.abort.abort()?,
        }
    };
    let join = stream
        .join
        .take()
        .expect("reader join handle was checked above");
    let joined = join.join();
    stream.state = ManagedHttpStreamState::Joined;
    if let Some(exit) = exit {
        stream.last_exit = Some(exit);
    }
    joined.map_err(|_| {
        EvaError::internal("MCP SSE reader thread panicked")
            .with_provider_code("mcp_stream_reader_panicked")
    })?;
    Ok(stream
        .last_exit
        .as_ref()
        .map_or("completed", ManagedReaderExit::as_str))
}

fn http_stream_report(
    adapter_id: &AdapterId,
    session_id: &str,
    stream_id: String,
    status: McpStreamStatus,
    mut audit: Vec<String>,
) -> McpStreamReport {
    let mut base = vec!["transport:mcp_streamable_http".to_owned()];
    base.append(&mut audit);
    McpStreamReport {
        adapter_id: adapter_id.clone(),
        session_id: session_id.to_owned(),
        stream_id,
        status,
        audit: base,
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
        McpStreamableHttpConfig,
    };
    use eva_core::ErrorKind;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::Instant;

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

    #[test]
    fn real_http_abort_closes_socket_deletes_session_joins_reader_and_clears_registry() {
        let (endpoint, server) = spawn_managed_http_fixture(vec![204], None);
        let session = ready_http_session(&endpoint);
        let mut registry = McpStreamableHttpSessionRegistry::new();
        let registered = registry
            .register_session(AdapterId::parse("managed-mcp").unwrap(), session)
            .unwrap();
        registry
            .open_event_stream(&registered.session_id, "events")
            .unwrap();

        assert_eq!(registry.dangling_session_count(), 1);
        assert_eq!(registry.dangling_reader_count(), 1);
        let started = Instant::now();
        let aborted = registry
            .abort_stream(&registered.session_id, "events")
            .unwrap();

        assert!(started.elapsed() < Duration::from_millis(1_500));
        assert_eq!(aborted.status, McpStreamStatus::Aborted);
        assert!(aborted
            .audit
            .contains(&"mcp.stream:socket_shutdown".to_owned()));
        assert!(aborted
            .audit
            .contains(&"mcp.http:session_delete_complete".to_owned()));
        assert_eq!(registry.dangling_session_count(), 0);
        assert_eq!(registry.dangling_reader_count(), 0);

        let (requests, stream_closed_before_delete) = server.join().unwrap();
        assert!(stream_closed_before_delete);
        assert_eq!(requests.len(), 4);
        assert!(requests[2].starts_with("GET /mcp HTTP/1.1\r\n"));
        assert!(requests[3].starts_with("DELETE /mcp HTTP/1.1\r\n"));
        assert!(requests[3].contains("Mcp-Session-Id: opaque-session-secret\r\n"));
        assert!(requests[3].contains("MCP-Protocol-Version: 2025-11-25\r\n"));
        let debug = format!("{registry:?}");
        assert!(!debug.contains("opaque-session-secret"));
        assert!(!aborted
            .audit
            .iter()
            .any(|entry| entry.contains("opaque-session-secret")));
    }

    #[test]
    fn failed_delete_keeps_only_retryable_session_cleanup_without_a_reader() {
        let (endpoint, server) = spawn_managed_http_fixture(vec![500, 204], None);
        let session = ready_http_session(&endpoint);
        let mut registry = McpStreamableHttpSessionRegistry::new();
        let registered = registry
            .register_session(AdapterId::parse("retry-mcp").unwrap(), session)
            .unwrap();
        registry
            .open_event_stream(&registered.session_id, "events")
            .unwrap();

        let error = registry
            .abort_stream(&registered.session_id, "events")
            .unwrap_err();
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("mcp_http_session_delete_failed")
        );
        assert_eq!(registry.dangling_session_count(), 1);
        assert_eq!(registry.dangling_reader_count(), 0);
        let fenced = registry
            .open_event_stream(&registered.session_id, "must-not-open")
            .unwrap_err();
        assert_eq!(
            fenced.provider_code().map(|code| code.as_str()),
            Some("mcp_http_registry_cleanup_pending")
        );
        let poll_fenced = registry
            .try_next_stream_item(&registered.session_id, "events")
            .unwrap_err();
        assert_eq!(
            poll_fenced.provider_code().map(|code| code.as_str()),
            Some("mcp_http_registry_cleanup_pending")
        );

        registry
            .abort_stream(&registered.session_id, "events")
            .unwrap();
        assert_eq!(registry.dangling_session_count(), 0);
        assert_eq!(registry.dangling_reader_count(), 0);
        let (requests, stream_closed_before_delete) = server.join().unwrap();
        assert!(stream_closed_before_delete);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("DELETE "))
                .count(),
            2
        );
    }

    #[test]
    fn completed_reader_is_distinct_from_an_empty_queue_and_is_reaped() {
        const EVENT: &str =
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"terminal-payload-secret\"}\n\n";
        let (endpoint, server) = spawn_managed_http_fixture(vec![204], Some(EVENT));
        let session = ready_http_session(&endpoint);
        let mut registry = McpStreamableHttpSessionRegistry::new();
        let registered = registry
            .register_session(AdapterId::parse("finite-mcp").unwrap(), session)
            .unwrap();
        registry
            .open_event_stream(&registered.session_id, "events")
            .unwrap();
        assert!(!format!("{registry:?}").contains("terminal-payload-secret"));

        let deadline = Instant::now() + Duration::from_secs(1);
        let item = loop {
            if let Some(item) = registry
                .try_next_stream_item(&registered.session_id, "events")
                .unwrap()
            {
                break item;
            }
            assert!(Instant::now() < deadline, "managed event was not delivered");
            thread::yield_now();
        };
        assert!(matches!(item, McpSseItem::Message(_)));
        let terminal = loop {
            match registry.try_next_stream_item(&registered.session_id, "events") {
                Ok(None) => {
                    assert!(
                        Instant::now() < deadline,
                        "reader terminal state was not reported"
                    );
                    thread::yield_now();
                }
                Err(error) => break error,
                Ok(Some(_)) => panic!("fixture emitted an unexpected second event"),
            }
        };
        assert_eq!(
            terminal.provider_code().map(|code| code.as_str()),
            Some("mcp_stream_reader_ended")
        );
        assert_eq!(registry.dangling_reader_count(), 0);
        assert_eq!(registry.dangling_session_count(), 1);

        registry.shutdown_session(&registered.session_id).unwrap();
        assert_eq!(registry.dangling_session_count(), 0);
        let (requests, stream_closed_before_delete) = server.join().unwrap();
        assert!(stream_closed_before_delete);
        assert_eq!(requests.len(), 4);
    }

    #[test]
    fn not_ready_session_retains_cleanup_owner_instead_of_being_dropped() {
        let config = McpStreamableHttpConfig::legacy_http("http://127.0.0.1:9/mcp").unwrap();
        let session =
            McpStreamableHttpSession::new(config, BTreeMap::new(), Duration::from_secs(1), 1024)
                .unwrap();
        let mut registry = McpStreamableHttpSessionRegistry::new();
        let registered = registry
            .register_session(AdapterId::parse("not-ready-mcp").unwrap(), session)
            .unwrap();

        assert_eq!(registered.status, McpSessionLifecycleStatus::Orphaned);
        assert_eq!(registry.dangling_session_count(), 1);
        let fenced = registry
            .open_event_stream(&registered.session_id, "must-not-open")
            .unwrap_err();
        assert_eq!(
            fenced.provider_code().map(|code| code.as_str()),
            Some("mcp_http_registry_cleanup_pending")
        );
        registry.shutdown_session(&registered.session_id).unwrap();
        assert_eq!(registry.dangling_session_count(), 0);
    }

    fn ready_http_session(endpoint: &str) -> McpStreamableHttpSession {
        let config = McpStreamableHttpConfig::legacy_http(endpoint).unwrap();
        let mut session =
            McpStreamableHttpSession::new(config, BTreeMap::new(), Duration::from_secs(2), 4096)
                .unwrap();
        session
            .initialize(
                1,
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\"}}",
            )
            .unwrap();
        session
            .notify("{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}")
            .unwrap();
        session
    }

    fn spawn_managed_http_fixture(
        delete_statuses: Vec<u16>,
        event_body: Option<&'static str>,
    ) -> (String, JoinHandle<(Vec<String>, bool)>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for request_index in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                let request = read_fixture_http_request(&mut socket);
                let response = if request_index == 0 {
                    fixture_http_response(
                        200,
                        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-11-25\"}}",
                        true,
                    )
                } else {
                    fixture_http_response(202, "", true)
                };
                socket.write_all(response.as_bytes()).unwrap();
                socket.flush().unwrap();
                requests.push(request);
            }

            let (mut event_socket, _) = listener.accept().unwrap();
            let event_request = read_fixture_http_request(&mut event_socket);
            let event_response = if let Some(body) = event_body {
                format!(
                    "HTTP/1.1 200 OK\r\nMcp-Session-Id: opaque-session-secret\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{body}",
                    body.len()
                )
            } else {
                "HTTP/1.1 200 OK\r\nMcp-Session-Id: opaque-session-secret\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n".to_owned()
            };
            event_socket.write_all(event_response.as_bytes()).unwrap();
            event_socket.flush().unwrap();
            requests.push(event_request);
            let (closed_sender, closed_receiver) = mpsc::sync_channel(1);
            let event_monitor = thread::spawn(move || {
                event_socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut byte = [0_u8; 1];
                let closed = match event_socket.read(&mut byte) {
                    Ok(0) => true,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::BrokenPipe
                                | std::io::ErrorKind::UnexpectedEof
                        ) =>
                    {
                        true
                    }
                    _ => false,
                };
                let _ = closed_sender.send(closed);
            });

            let mut stream_closed_before_delete = false;
            for (index, status) in delete_statuses.into_iter().enumerate() {
                let (mut delete_socket, _) = listener.accept().unwrap();
                let delete_request = read_fixture_http_request(&mut delete_socket);
                if index == 0 {
                    stream_closed_before_delete = closed_receiver
                        .recv_timeout(Duration::from_secs(1))
                        .unwrap_or(false);
                }
                let response = fixture_http_response(status, "", false);
                delete_socket.write_all(response.as_bytes()).unwrap();
                delete_socket.flush().unwrap();
                requests.push(delete_request);
            }
            event_monitor.join().unwrap();
            (requests, stream_closed_before_delete)
        });
        (endpoint, server)
    }

    fn fixture_http_response(status: u16, body: &str, include_session: bool) -> String {
        let reason = match status {
            200 => "OK",
            202 => "Accepted",
            204 => "No Content",
            500 => "Internal Server Error",
            _ => "Fixture",
        };
        let session = if include_session {
            "Mcp-Session-Id: opaque-session-secret\r\n"
        } else {
            ""
        };
        let content_type = if body.is_empty() {
            ""
        } else {
            "Content-Type: application/json\r\n"
        };
        format!(
            "HTTP/1.1 {status} {reason}\r\n{session}{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn read_fixture_http_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "fixture request ended before its body");
            bytes.extend_from_slice(&buffer[..read]);
            let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let body_start = header_end + 4;
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if bytes.len() >= body_start + content_length {
                return String::from_utf8(bytes).unwrap();
            }
        }
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
