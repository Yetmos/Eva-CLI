//! MCP session registry and lifecycle supervisor boundary.

use crate::session::{
    McpServerTransport, McpSession, McpSessionConfig, McpSessionManager, McpSessionSupervisor,
};
use eva_core::{AdapterId, EvaError};
use std::collections::{BTreeMap, BTreeSet};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "MCP session registry, health, stream abort, and orphan cleanup";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpSessionLifecycleStatus {
    Running,
    Stopped,
    Orphaned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpStreamStatus {
    Running,
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSessionLifecycleReport {
    pub adapter_id: AdapterId,
    pub session_id: String,
    pub status: McpSessionLifecycleStatus,
    pub process_id: Option<u32>,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSessionHealthReport {
    pub adapter_id: AdapterId,
    pub session_id: String,
    pub status: McpSessionLifecycleStatus,
    pub healthy: bool,
    pub active_streams: Vec<String>,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpRegisteredSession {
    pub adapter_id: AdapterId,
    pub session_id: String,
    pub process_id: Option<u32>,
    pub server_transport: McpServerTransport,
    pub explicit_tools: Vec<String>,
    pub active_streams: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStreamReport {
    pub adapter_id: AdapterId,
    pub session_id: String,
    pub stream_id: String,
    pub status: McpStreamStatus,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpOrphanCleanupReport {
    pub removed_sessions: Vec<String>,
    pub audit: Vec<String>,
}

pub trait McpProcessInspector {
    fn process_is_running(&self, process_id: u32) -> bool;
}

#[derive(Debug, Default)]
pub struct McpSessionRegistry {
    sessions: BTreeMap<String, RegisteredSession>,
}

#[derive(Debug)]
struct RegisteredSession {
    session: McpSession,
    process_id: Option<u32>,
    server_transport: McpServerTransport,
    explicit_tools: BTreeSet<String>,
    streams: BTreeMap<String, McpStreamStatus>,
}

impl McpSessionLifecycleStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Orphaned => "orphaned",
        }
    }
}

impl McpStreamStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Aborted => "aborted",
        }
    }
}

impl McpSessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

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

    fn session(&self, session_id: &str) -> Result<&RegisteredSession, EvaError> {
        self.sessions.get(session_id).ok_or_else(|| {
            EvaError::not_found("MCP session is not registered")
                .with_context("session_id", session_id)
        })
    }

    fn session_mut(&mut self, session_id: &str) -> Result<&mut RegisteredSession, EvaError> {
        self.sessions.get_mut(session_id).ok_or_else(|| {
            EvaError::not_found("MCP session is not registered")
                .with_context("session_id", session_id)
        })
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{
        McpProcessHandle, McpProcessShutdownRequest, McpProcessSpec, McpProcessStartRequest,
    };
    use eva_core::ErrorKind;

    #[derive(Debug, Clone)]
    struct FakeSupervisor {
        next_session_id: String,
        process_id: Option<u32>,
        shutdown_calls: Vec<String>,
    }

    impl FakeSupervisor {
        fn new(session_id: &str, process_id: Option<u32>) -> Self {
            Self {
                next_session_id: session_id.to_owned(),
                process_id,
                shutdown_calls: Vec::new(),
            }
        }
    }

    impl McpSessionSupervisor for FakeSupervisor {
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

        fn shutdown_process(
            &mut self,
            request: &McpProcessShutdownRequest,
        ) -> Result<(), EvaError> {
            self.shutdown_calls.push(request.session_id.clone());
            Ok(())
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct StaticInspector {
        running: bool,
    }

    impl McpProcessInspector for StaticInspector {
        fn process_is_running(&self, _process_id: u32) -> bool {
            self.running
        }
    }

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
