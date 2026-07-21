//! 管理外部提供者的准入、会话凭据和进程快照。
//!
//! `acquire` 在写入运行中快照前依次检查熔断、并发与速率限制，任一检查失败都不会占用
//! 执行槽；`complete` 负责释放槽并根据结果推进熔断状态。会话令牌只注入对应提供者的
//! 环境变量或请求头，审计和输出路径必须使用摘要或脱敏值。
//! Provider supervisor slots and process table integration.

#[cfg(test)]
use crate::credential_vault::{
    credential_finalizer_test_guard, drain_credential_finalizer_while_test_guarded,
};
use crate::credential_vault::{
    drain_deferred_credential_releases_until, track_running_credential_finalizer_owner,
    CredentialFinalizerRunningGuard, CredentialSessionLease,
};
use crate::manifest::{AdapterCircuitBreaker, AdapterHandle, AdapterRateLimit};
use crate::process_backend::{OsProcessBackend, ProcessIdentity, ProcessTerminationOutcome};
use crate::runtime::AdapterInvocation;
use eva_config::{AdapterTransport, ProviderConfig, ProviderRunAsIdentity};
use eva_core::{AdapterId, CapabilityName, ErrorKind, EvaError, RequestId};
use eva_mcp::{
    McpHttpDrainReport, McpJsonRpcCallReport, McpJsonRpcClient, McpSessionLifecycleReport,
    McpStreamableHttpSession, McpStreamableHttpSessionRegistry,
};
use eva_storage::{
    FileSystemProviderAdmissionTable, FileSystemProviderProcessTable, InMemoryProviderProcessTable,
    ProviderProcessSnapshot, ProviderProcessTable,
};
use std::collections::BTreeMap;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, TryLockError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "provider execution slots and process table mutation";
/// 定义 `PROVIDER_SESSION_ID_ENV` 常量。
pub const PROVIDER_SESSION_ID_ENV: &str = "EVA_PROVIDER_SESSION_ID";
/// 定义 `PROVIDER_SESSION_TOKEN_ENV` 常量。
pub const PROVIDER_SESSION_TOKEN_ENV: &str = "EVA_PROVIDER_SESSION_TOKEN";
/// 定义 `PROVIDER_SESSION_ID_HEADER` 常量。
pub const PROVIDER_SESSION_ID_HEADER: &str = "X-Eva-Provider-Session";
/// 定义 `PROVIDER_SESSION_TOKEN_HEADER` 常量。
pub const PROVIDER_SESSION_TOKEN_HEADER: &str = "X-Eva-Provider-Session-Token";
const PROVIDER_ADMISSION_RESERVATION_AUDIT_PREFIX: &str = "provider.admission:reservation:";
const PROVIDER_ADMISSION_RELEASE_PENDING_AUDIT_PREFIX: &str = "provider.admission:release_pending:";
const PROVIDER_ADMISSION_RELEASE_RESOLVED_AUDIT_PREFIX: &str =
    "provider.admission:release_resolved:";

/// Bounded timing for closing provider admission and retiring active slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderDrainOptions {
    total_timeout: Duration,
    poll_interval: Duration,
}

impl ProviderDrainOptions {
    /// Creates a provider drain budget. Both values must be non-zero so a
    /// caller cannot accidentally skip the admission-close observation step.
    pub fn new(total_timeout: Duration) -> Result<Self, EvaError> {
        if total_timeout.is_zero() {
            return Err(EvaError::invalid_argument(
                "provider drain timeout must be greater than zero",
            ));
        }
        Ok(Self {
            total_timeout,
            poll_interval: Duration::from_millis(10).min(total_timeout),
        })
    }

    /// Returns the absolute wall-clock budget for the drain operation.
    pub const fn total_timeout(self) -> Duration {
        self.total_timeout
    }

    /// Overrides the bounded polling interval, primarily for deterministic tests.
    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Result<Self, EvaError> {
        if poll_interval.is_zero() {
            return Err(EvaError::invalid_argument(
                "provider drain poll interval must be greater than zero",
            ));
        }
        self.poll_interval = poll_interval.min(self.total_timeout);
        Ok(self)
    }

    const fn poll_interval(self) -> Duration {
        self.poll_interval
    }
}

impl Default for ProviderDrainOptions {
    fn default() -> Self {
        Self::new(Duration::from_secs(3)).expect("static provider drain budget is valid")
    }
}

/// Stable evidence emitted after a supervisor has closed admission and retired
/// every provider snapshot it could safely own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDrainReport {
    /// Whether this report is a cached result of an earlier successful drain.
    pub already_drained: bool,
    /// `drained` when no active provider remains, `timed_out` otherwise.
    pub phase: String,
    /// Number of active snapshots still present when the report was produced.
    pub active_provider_count: usize,
    /// Number of provider process boundaries terminated during this drain.
    pub terminated_provider_count: usize,
    /// Number of boundaries that required force termination.
    pub forced_provider_count: usize,
    /// Number of legacy snapshots without an OS identity.
    pub missing_identity_count: usize,
    /// Streamable HTTP application sessions owned when the drain began.
    pub mcp_http_sessions_before: usize,
    /// Streamable HTTP reader threads owned when the drain began.
    pub mcp_http_readers_before: usize,
    /// Streamable HTTP application sessions left after the drain.
    pub mcp_http_sessions_after: usize,
    /// Streamable HTTP reader threads left after the drain.
    pub mcp_http_readers_after: usize,
    /// Streamable HTTP sessions fenced for cleanup after the drain.
    pub mcp_http_cleanup_pending_after: usize,
    /// Deterministic lifecycle evidence suitable for daemon audit output.
    pub audit: Vec<String>,
}

/// Shared pointer-identity handle for the daemon-owned Streamable HTTP
/// registry. Cloned supervisors address the same lifecycle authority without
/// comparing or formatting opaque server-issued session material.
#[derive(Clone)]
pub(crate) struct McpHttpLifecycleHandle(Arc<McpHttpLifecycleShared>);

struct McpHttpLifecycleShared {
    state: Mutex<McpHttpLifecycleState>,
    admission_closed: AtomicBool,
    operation_gate: Mutex<McpHttpOperationGate>,
    operations_idle: Condvar,
    drain_lock: Mutex<()>,
}

struct McpHttpOperationGate {
    active_operations: usize,
}

#[derive(Debug)]
pub(crate) struct McpHttpOperationPermit {
    lifecycle: McpHttpLifecycleHandle,
}

pub(crate) struct McpHttpLifecycleState {
    registry: McpStreamableHttpSessionRegistry,
    credential_leases: BTreeMap<String, CredentialSessionLease>,
    credential_releases: BTreeMap<String, CredentialReleaseTask>,
    next_detached_credential_id: u64,
}

struct CredentialReleaseTask {
    completion: Arc<CredentialReleaseCompletion>,
    worker: Arc<CredentialReleaseWorker>,
}

struct CredentialReleaseWorker {
    handle: Mutex<Option<thread::JoinHandle<()>>>,
    start_gate: Arc<CredentialReleaseStartGate>,
    finalizer_scope: u64,
}

struct CredentialReleaseStartGate {
    attached: Mutex<bool>,
    ready: Condvar,
}

struct DeferredCredentialJoin {
    join: thread::JoinHandle<()>,
    acknowledgement: Option<mpsc::SyncSender<bool>>,
    _credential_residual: Option<CredentialFinalizerRunningGuard>,
}

struct CredentialJoinReaper {
    sender: mpsc::Sender<DeferredCredentialJoin>,
    worker: thread::JoinHandle<()>,
}

static CREDENTIAL_JOIN_REAPER: OnceLock<CredentialJoinReaper> = OnceLock::new();

struct CredentialReleaseCompletion {
    state: Mutex<CredentialReleaseCompletionState>,
    ready: Condvar,
}

enum CredentialReleaseCompletionState {
    Pending(CredentialSessionLease),
    Running,
    Finished {
        credentials: CredentialSessionLease,
        result: Result<Vec<String>, EvaError>,
    },
}

struct PreparedCredentialRelease {
    waiter: CredentialReleaseWaiter,
}

#[derive(Clone)]
struct CredentialReleaseWaiter {
    session_id: String,
    completion: Arc<CredentialReleaseCompletion>,
    worker: Arc<CredentialReleaseWorker>,
}

impl CredentialReleaseWaiter {
    fn wait_until(
        &self,
        deadline: Option<Instant>,
    ) -> Result<Result<Vec<String>, EvaError>, EvaError> {
        let mut state = self.completion.state.lock().map_err(|_| {
            EvaError::internal("credential release completion lock is poisoned")
                .with_provider_code("credential_release_completion_lock_poisoned")
        })?;
        loop {
            if let CredentialReleaseCompletionState::Finished { result, .. } = &*state {
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    return Err(credential_release_timeout());
                }
                return Ok(result.clone());
            }
            state = match deadline {
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(credential_release_timeout());
                    }
                    let (next, wait) = self
                        .completion
                        .ready
                        .wait_timeout(state, remaining)
                        .map_err(|_| {
                            EvaError::internal("credential release completion lock is poisoned")
                                .with_provider_code("credential_release_completion_lock_poisoned")
                        })?;
                    if wait.timed_out()
                        && !matches!(&*next, CredentialReleaseCompletionState::Finished { .. })
                    {
                        return Err(credential_release_timeout());
                    }
                    next
                }
                None => self.completion.ready.wait(state).map_err(|_| {
                    EvaError::internal("credential release completion lock is poisoned")
                        .with_provider_code("credential_release_completion_lock_poisoned")
                })?,
            };
        }
    }
}

fn credential_release_timeout() -> EvaError {
    EvaError::timeout("provider credential release exceeded the drain deadline")
        .with_provider_code("credential_release_timeout")
        .with_context("cleanup_blocked", "true")
}

fn mcp_http_operation_drain_timeout(active_operations: usize) -> EvaError {
    EvaError::timeout("MCP HTTP active operations exceeded the drain deadline")
        .with_provider_code("mcp_http_operation_drain_timeout")
        .with_context("active_operations", active_operations.to_string())
        .with_context("cleanup_blocked", (active_operations != 0).to_string())
}

fn mcp_http_operation_gate_drain_timeout() -> EvaError {
    EvaError::timeout("MCP HTTP operation gate exceeded the drain deadline")
        .with_provider_code("mcp_http_operation_drain_timeout")
        .with_context("active_operations", "unknown")
        .with_context("operation_gate_busy", "true")
        .with_context("cleanup_blocked", "true")
}

fn mcp_http_admission_closed() -> EvaError {
    EvaError::conflict("MCP Streamable HTTP registry admission is closed")
        .with_provider_code("mcp_http_registry_admission_closed")
}

impl Drop for CredentialReleaseWorker {
    fn drop(&mut self) {
        let Some(join) = self
            .handle
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        else {
            return;
        };
        if join.is_finished() {
            let _ = join.join();
        } else {
            let residual = track_running_credential_finalizer_owner(self.finalizer_scope);
            defer_credential_join(join, None, residual);
        }
    }
}

fn credential_join_reaper() -> Result<&'static CredentialJoinReaper, EvaError> {
    if let Some(reaper) = CREDENTIAL_JOIN_REAPER.get() {
        return Ok(reaper);
    }

    let (sender, receiver) = mpsc::channel();
    let worker = thread::Builder::new()
        .name("eva-credential-release-reaper".to_owned())
        .spawn(move || run_credential_join_reaper(receiver))
        .map_err(|error| {
            EvaError::unavailable("failed to start credential release join reaper")
                .with_provider_code("credential_release_reaper_spawn_failed")
                .with_context("os_error_kind", format!("{:?}", error.kind()))
        })?;
    let candidate = CredentialJoinReaper { sender, worker };
    if let Err(candidate) = CREDENTIAL_JOIN_REAPER.set(candidate) {
        drop(candidate.sender);
        let _ = candidate.worker.join();
    }
    CREDENTIAL_JOIN_REAPER.get().ok_or_else(|| {
        EvaError::internal("credential release join reaper initialization was lost")
            .with_provider_code("credential_release_reaper_missing")
    })
}

fn run_credential_join_reaper(receiver: Receiver<DeferredCredentialJoin>) {
    let mut pending = Vec::new();
    let mut disconnected = false;
    while !disconnected || !pending.is_empty() {
        if !disconnected {
            match receiver.recv_timeout(Duration::from_millis(10)) {
                Ok(task) => pending.push(task),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => disconnected = true,
            }
            loop {
                match receiver.try_recv() {
                    Ok(task) => pending.push(task),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        } else {
            thread::sleep(Duration::from_millis(10));
        }

        let mut index = 0;
        while index < pending.len() {
            if pending[index].join.is_finished() {
                let task = pending.swap_remove(index);
                let joined = task.join.join().is_ok();
                if let Some(acknowledgement) = task.acknowledgement {
                    let _ = acknowledgement.send(joined);
                }
            } else {
                index += 1;
            }
        }
    }
}

fn defer_credential_join(
    join: thread::JoinHandle<()>,
    acknowledgement: Option<mpsc::SyncSender<bool>>,
    credential_residual: Option<CredentialFinalizerRunningGuard>,
) {
    let task = DeferredCredentialJoin {
        join,
        acknowledgement,
        _credential_residual: credential_residual,
    };
    let Some(reaper) = credential_join_reaper().ok() else {
        if let Some(acknowledgement) = &task.acknowledgement {
            let _ = acknowledgement.send(false);
        }
        let _ = Box::leak(Box::new(task));
        return;
    };
    if let Err(error) = reaper.sender.send(task) {
        let task = error.0;
        if let Some(acknowledgement) = &task.acknowledgement {
            let _ = acknowledgement.send(false);
        }
        let _ = Box::leak(Box::new(task));
    }
}

impl McpHttpLifecycleState {
    fn register_starting_session(
        &mut self,
        adapter_id: AdapterId,
        session: McpStreamableHttpSession,
        credentials: CredentialSessionLease,
    ) -> Result<McpSessionLifecycleReport, (EvaError, PreparedCredentialRelease)> {
        let registered = match self.registry.register_starting_session(adapter_id, session) {
            Ok(registered) => registered,
            Err(error) => {
                let release = self.prepare_detached_credential_release(credentials);
                return Err((error, release));
            }
        };
        if self.credential_leases.contains_key(&registered.session_id) {
            let _ = self.registry.shutdown_session(&registered.session_id);
            return Err((
                EvaError::internal("MCP HTTP lifecycle credential owner ID collided")
                    .with_provider_code("mcp_http_credential_owner_collision"),
                self.prepare_detached_credential_release(credentials),
            ));
        }
        self.credential_leases
            .insert(registered.session_id.clone(), credentials);
        Ok(registered)
    }

    fn call_tool(
        &mut self,
        session_id: &str,
        client: &McpJsonRpcClient,
        request_id: RequestId,
        tool: &str,
        input: &str,
    ) -> Result<McpJsonRpcCallReport, EvaError> {
        self.registry
            .call_tool(session_id, client, request_id, tool, input)
    }

    fn shutdown_session(
        &mut self,
        session_id: &str,
    ) -> Result<(McpSessionLifecycleReport, PreparedCredentialRelease), EvaError> {
        let report = self.registry.shutdown_session(session_id)?;
        let release = self.prepare_credential_release(session_id)?;
        Ok((report, release))
    }

    fn prepare_credential_release(
        &mut self,
        session_id: &str,
    ) -> Result<PreparedCredentialRelease, EvaError> {
        let credentials = self.credential_leases.remove(session_id).ok_or_else(|| {
            EvaError::internal("MCP HTTP session lost its credential lifecycle owner")
                .with_provider_code("mcp_http_credential_owner_missing")
        })?;
        Ok(self.prepare_credential_release_owner(session_id.to_owned(), credentials))
    }

    fn prepare_detached_credential_release(
        &mut self,
        credentials: CredentialSessionLease,
    ) -> PreparedCredentialRelease {
        loop {
            self.next_detached_credential_id = self.next_detached_credential_id.wrapping_add(1);
            let session_id = format!(
                "mcp-http-detached-credential-{}",
                self.next_detached_credential_id
            );
            if !self.credential_leases.contains_key(&session_id)
                && !self.credential_releases.contains_key(&session_id)
            {
                return self.prepare_credential_release_owner(session_id, credentials);
            }
        }
    }

    fn prepare_credential_release_owner(
        &mut self,
        session_id: String,
        credentials: CredentialSessionLease,
    ) -> PreparedCredentialRelease {
        let finalizer_scope = credentials.release_finalizer_scope();
        let completion = Arc::new(CredentialReleaseCompletion {
            state: Mutex::new(CredentialReleaseCompletionState::Pending(credentials)),
            ready: Condvar::new(),
        });
        let start_gate = Arc::new(CredentialReleaseStartGate {
            attached: Mutex::new(false),
            ready: Condvar::new(),
        });
        let worker = Arc::new(CredentialReleaseWorker {
            handle: Mutex::new(None),
            start_gate,
            finalizer_scope,
        });
        self.credential_releases.insert(
            session_id.clone(),
            CredentialReleaseTask {
                completion: Arc::clone(&completion),
                worker: Arc::clone(&worker),
            },
        );
        PreparedCredentialRelease {
            waiter: CredentialReleaseWaiter {
                session_id,
                completion,
                worker,
            },
        }
    }

    fn prepare_completed_credential_releases(&mut self) {
        let completed = self
            .credential_leases
            .keys()
            .filter(|session_id| !self.registry.contains_session(session_id))
            .cloned()
            .collect::<Vec<_>>();
        for session_id in completed {
            let _ = self.prepare_credential_release(&session_id);
        }
    }

    fn credential_release_work(
        &self,
    ) -> (Vec<PreparedCredentialRelease>, Vec<CredentialReleaseWaiter>) {
        let mut pending = Vec::new();
        let mut running = Vec::new();
        for (session_id, task) in &self.credential_releases {
            let waiter = CredentialReleaseWaiter {
                session_id: session_id.clone(),
                completion: Arc::clone(&task.completion),
                worker: Arc::clone(&task.worker),
            };
            let launched = task
                .worker
                .handle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_some();
            if launched {
                running.push(waiter);
            } else {
                pending.push(PreparedCredentialRelease { waiter });
            }
        }
        (pending, running)
    }
}

impl fmt::Debug for McpHttpLifecycleState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpHttpLifecycleState")
            .field("registry", &self.registry)
            .field("credential_owner_count", &self.credential_leases.len())
            .field(
                "credential_release_task_count",
                &self.credential_releases.len(),
            )
            .finish()
    }
}

impl Drop for McpHttpOperationPermit {
    fn drop(&mut self) {
        let mut gate = self
            .lifecycle
            .0
            .operation_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        debug_assert!(gate.active_operations > 0);
        gate.active_operations = gate.active_operations.saturating_sub(1);
        if gate.active_operations == 0 {
            self.lifecycle.0.operations_idle.notify_all();
        }
    }
}

impl McpHttpLifecycleHandle {
    pub(crate) fn new() -> Self {
        Self(Arc::new(McpHttpLifecycleShared {
            state: Mutex::new(McpHttpLifecycleState {
                registry: McpStreamableHttpSessionRegistry::new(),
                credential_leases: BTreeMap::new(),
                credential_releases: BTreeMap::new(),
                next_detached_credential_id: 0,
            }),
            admission_closed: AtomicBool::new(false),
            operation_gate: Mutex::new(McpHttpOperationGate {
                active_operations: 0,
            }),
            operations_idle: Condvar::new(),
            drain_lock: Mutex::new(()),
        }))
    }

    pub(crate) fn begin_operation(&self) -> Result<McpHttpOperationPermit, EvaError> {
        if self.0.admission_closed.load(Ordering::Acquire) {
            return Err(mcp_http_admission_closed());
        }
        let mut gate = self.0.operation_gate.lock().map_err(|_| {
            EvaError::internal("MCP HTTP operation gate lock is poisoned")
                .with_provider_code("mcp_http_operation_gate_lock_poisoned")
        })?;
        if self.0.admission_closed.load(Ordering::Acquire) {
            return Err(mcp_http_admission_closed());
        }
        gate.active_operations = gate.active_operations.checked_add(1).ok_or_else(|| {
            EvaError::conflict("MCP HTTP active operation count is exhausted")
                .with_provider_code("mcp_http_operation_count_exhausted")
        })?;
        Ok(McpHttpOperationPermit {
            lifecycle: self.clone(),
        })
    }

    pub(crate) fn register_starting_session(
        &self,
        operation: &McpHttpOperationPermit,
        adapter_id: AdapterId,
        session: McpStreamableHttpSession,
        credentials: CredentialSessionLease,
    ) -> Result<McpSessionLifecycleReport, EvaError> {
        if let Err(error) = self.ensure_operation_owner(operation) {
            let release = operation
                .lifecycle
                .release_detached_credentials_unchecked(credentials);
            return Err(match release {
                Ok(_) => error,
                Err(release_error) => {
                    error.with_context("credential_release_error", release_error.to_string())
                }
            });
        }
        let registration = match self.try_lock() {
            Ok(mut lifecycle) => {
                lifecycle.register_starting_session(adapter_id, session, credentials)
            }
            Err(error) => {
                let release = self.release_detached_credentials(operation, credentials);
                return Err(match release {
                    Ok(_) => error,
                    Err(release_error) => {
                        error.with_context("credential_release_error", release_error.to_string())
                    }
                });
            }
        };
        match registration {
            Ok(report) => Ok(report),
            Err((error, prepared)) => {
                let release = self
                    .launch_credential_release(prepared, None)
                    .and_then(|waiter| self.complete_credential_release(&waiter, None));
                Err(match release {
                    Ok(_) => error,
                    Err(release_error) => {
                        error.with_context("credential_release_error", release_error.to_string())
                    }
                })
            }
        }
    }

    pub(crate) fn call_tool(
        &self,
        operation: &McpHttpOperationPermit,
        session_id: &str,
        client: &McpJsonRpcClient,
        request_id: RequestId,
        tool: &str,
        input: &str,
    ) -> Result<McpJsonRpcCallReport, EvaError> {
        self.ensure_operation_owner(operation)?;
        self.try_lock()?
            .call_tool(session_id, client, request_id, tool, input)
    }

    pub(crate) fn shutdown_session(
        &self,
        operation: &McpHttpOperationPermit,
        session_id: &str,
    ) -> Result<(McpSessionLifecycleReport, Vec<String>), EvaError> {
        self.ensure_operation_owner(operation)?;
        let (report, prepared) = self.try_lock()?.shutdown_session(session_id)?;
        let waiter = self.launch_credential_release(prepared, None)?;
        let audit = self.complete_credential_release(&waiter, None)?;
        Ok((report, audit))
    }

    pub(crate) fn release_detached_credentials(
        &self,
        operation: &McpHttpOperationPermit,
        credentials: CredentialSessionLease,
    ) -> Result<Vec<String>, EvaError> {
        if let Err(error) = self.ensure_operation_owner(operation) {
            let release = operation
                .lifecycle
                .release_detached_credentials_unchecked(credentials);
            return Err(match release {
                Ok(_) => error,
                Err(release_error) => {
                    error.with_context("credential_release_error", release_error.to_string())
                }
            });
        }
        self.release_detached_credentials_unchecked(credentials)
    }

    fn release_detached_credentials_unchecked(
        &self,
        credentials: CredentialSessionLease,
    ) -> Result<Vec<String>, EvaError> {
        let prepared = self
            .0
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .prepare_detached_credential_release(credentials);
        let waiter = self.launch_credential_release(prepared, None)?;
        self.complete_credential_release(&waiter, None)
    }

    fn ensure_operation_owner(&self, operation: &McpHttpOperationPermit) -> Result<(), EvaError> {
        if Arc::ptr_eq(&self.0, &operation.lifecycle.0) {
            return Ok(());
        }
        Err(
            EvaError::conflict("MCP HTTP operation permit belongs to a different lifecycle")
                .with_provider_code("mcp_http_operation_permit_mismatch"),
        )
    }

    fn launch_credential_release(
        &self,
        prepared: PreparedCredentialRelease,
        callback_deadline: Option<Instant>,
    ) -> Result<CredentialReleaseWaiter, EvaError> {
        let waiter = prepared.waiter;
        credential_join_reaper()?;
        let worker_completion = Arc::clone(&waiter.completion);
        let start_gate = Arc::clone(&waiter.worker.start_gate);
        let worker = thread::Builder::new()
            .name("eva-credential-release".to_owned())
            .spawn(move || {
                let mut attached = start_gate
                    .attached
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                while !*attached {
                    attached = start_gate
                        .ready
                        .wait(attached)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                drop(attached);
                let mut credentials = {
                    let mut state = worker_completion
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    match std::mem::replace(&mut *state, CredentialReleaseCompletionState::Running)
                    {
                        CredentialReleaseCompletionState::Pending(credentials) => credentials,
                        unexpected => {
                            *state = unexpected;
                            return;
                        }
                    }
                };
                let result = if callback_deadline.is_some_and(|deadline| Instant::now() >= deadline)
                {
                    Err(credential_release_timeout())
                } else {
                    match catch_unwind(AssertUnwindSafe(|| credentials.release())) {
                        Ok(Ok(())) => Ok(credentials.audit_entries()),
                        Ok(Err(error)) => Err(error),
                        Err(_) => Err(EvaError::internal(
                            "provider credential release callback panicked",
                        )
                        .with_provider_code("credential_release_callback_panicked")),
                    }
                };
                {
                    let mut state = worker_completion
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    *state = CredentialReleaseCompletionState::Finished {
                        credentials,
                        result,
                    };
                }
                worker_completion.ready.notify_all();
            });

        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                return Err(EvaError::unavailable(
                    "provider credential release worker could not start",
                )
                .with_provider_code("credential_release_worker_spawn_failed")
                .with_context("os_error_kind", format!("{:?}", error.kind())));
            }
        };

        let mut worker_slot = waiter
            .worker
            .handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        debug_assert!(worker_slot.is_none());
        *worker_slot = Some(worker);
        drop(worker_slot);
        let mut attached = waiter
            .worker
            .start_gate
            .attached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *attached = true;
        drop(attached);
        waiter.worker.start_gate.ready.notify_all();
        Ok(waiter)
    }

    fn complete_credential_release(
        &self,
        waiter: &CredentialReleaseWaiter,
        deadline: Option<Instant>,
    ) -> Result<Vec<String>, EvaError> {
        let observed = waiter.wait_until(deadline)?;
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(credential_release_timeout());
        }
        loop {
            let mut lifecycle = match self.0.state.try_lock() {
                Ok(lifecycle) => lifecycle,
                Err(TryLockError::WouldBlock) => {
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        return Err(credential_release_timeout());
                    }
                    thread::yield_now();
                    continue;
                }
                Err(TryLockError::Poisoned(_)) => {
                    return Err(EvaError::internal(
                        "MCP Streamable HTTP lifecycle registry lock is poisoned",
                    )
                    .with_provider_code("mcp_http_registry_lock_poisoned"));
                }
            };
            let Some(task) = lifecycle.credential_releases.get(&waiter.session_id) else {
                return observed;
            };
            if !Arc::ptr_eq(&task.completion, &waiter.completion) {
                return Err(EvaError::internal(
                    "credential release completion owner changed unexpectedly",
                )
                .with_provider_code("credential_release_owner_mismatch"));
            }
            let worker_finished = task
                .worker
                .handle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
                .is_some_and(thread::JoinHandle::is_finished);
            if !worker_finished {
                drop(lifecycle);
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    return Err(credential_release_timeout());
                }
                thread::yield_now();
                continue;
            }

            let task = lifecycle
                .credential_releases
                .remove(&waiter.session_id)
                .expect("finished credential release task remains registered");
            let worker = task
                .worker
                .handle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
                .expect("finished credential release task owns its worker");
            let join_result = worker.join();
            let mut state = waiter.completion.state.lock().map_err(|_| {
                EvaError::internal("credential release completion lock is poisoned")
                    .with_provider_code("credential_release_completion_lock_poisoned")
            })?;
            let (credentials, mut result) =
                match std::mem::replace(&mut *state, CredentialReleaseCompletionState::Running) {
                    CredentialReleaseCompletionState::Finished {
                        credentials,
                        result,
                    } => (credentials, result),
                    unexpected => {
                        *state = unexpected;
                        return Err(EvaError::internal(
                            "credential release worker finished without an outcome",
                        )
                        .with_provider_code("credential_release_outcome_missing"));
                    }
                };
            if join_result.is_err() {
                result = Err(
                    EvaError::internal("provider credential release worker panicked")
                        .with_provider_code("credential_release_worker_panicked"),
                );
            }
            let deadline_elapsed = deadline.is_some_and(|deadline| Instant::now() >= deadline);
            return match result {
                Ok(_) if deadline_elapsed => Err(credential_release_timeout()),
                Ok(audit) => Ok(audit),
                Err(error) => {
                    lifecycle
                        .credential_leases
                        .insert(waiter.session_id.clone(), credentials);
                    if deadline_elapsed {
                        Err(credential_release_timeout())
                    } else {
                        Err(error)
                    }
                }
            };
        }
    }

    pub(crate) fn drain_all_until(
        &self,
        deadline: Instant,
    ) -> Result<McpHttpDrainReport, EvaError> {
        let _drain_guard = self.drain_guard_until(deadline)?;
        self.close_operation_admission_and_wait_until(deadline)?;

        let (registry_drain, prepared, existing_waiters) = {
            let mut lifecycle = self.lock_until(deadline)?;
            let registry_drain = lifecycle.registry.drain_all_until(deadline);
            lifecycle.prepare_completed_credential_releases();
            let (prepared, existing_waiters) = lifecycle.credential_release_work();
            (registry_drain, prepared, existing_waiters)
        };

        let mut release_error = None;
        let mut waiters = existing_waiters;
        let mut prepared = prepared.into_iter();
        for next in prepared.by_ref() {
            if Instant::now() >= deadline {
                if release_error.is_none() {
                    release_error = Some(credential_release_timeout());
                }
                break;
            }
            match self.launch_credential_release(next, Some(deadline)) {
                Ok(waiter) => waiters.push(waiter),
                Err(error) if release_error.is_none() => release_error = Some(error),
                Err(_) => {}
            }
        }
        for waiter in waiters {
            if Instant::now() >= deadline {
                if release_error.is_none() {
                    release_error = Some(credential_release_timeout());
                }
                break;
            }
            if let Err(error) = self.complete_credential_release(&waiter, Some(deadline)) {
                if release_error.is_none() {
                    release_error = Some(error);
                }
            }
            if Instant::now() >= deadline {
                break;
            }
        }

        if let Err(error) = drain_deferred_credential_releases_until(deadline) {
            if release_error.is_none() {
                release_error = Some(error);
            }
        }

        let residual = self.lifecycle_residual_until(deadline);
        match residual {
            Ok((0, 0, 0)) => {}
            Ok((active, leases, releases)) if release_error.is_none() => {
                release_error = Some(
                    EvaError::unavailable("provider credential drain left lifecycle owners behind")
                        .with_provider_code("credential_release_drain_incomplete")
                        .with_context("active_operations_after", active.to_string())
                        .with_context("credential_leases_after", leases.to_string())
                        .with_context("credential_releases_after", releases.to_string()),
                );
            }
            Ok(_) => {}
            Err(error) if release_error.is_none() => release_error = Some(error),
            Err(_) => {}
        }

        match (registry_drain, release_error) {
            (Ok(_), None) if Instant::now() >= deadline => Err(credential_release_timeout()),
            (Ok(report), None) => Ok(report),
            (Err(error), None) => Err(error),
            (Ok(_), Some(error)) => {
                Err(error.with_context("provider_drain_phase", "credential_release"))
            }
            (Err(error), Some(release_error)) => {
                Err(error.with_context("credential_release_error", release_error.to_string()))
            }
        }
    }

    fn drain_guard_until(&self, deadline: Instant) -> Result<MutexGuard<'_, ()>, EvaError> {
        loop {
            match self.0.drain_lock.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Err(mcp_http_operation_drain_timeout(0));
                    }
                    thread::yield_now();
                }
                Err(TryLockError::Poisoned(_)) => {
                    return Err(EvaError::internal("MCP HTTP drain lock is poisoned")
                        .with_provider_code("mcp_http_drain_lock_poisoned"));
                }
            }
        }
    }

    fn close_operation_admission_and_wait_until(&self, deadline: Instant) -> Result<(), EvaError> {
        self.0.admission_closed.store(true, Ordering::Release);
        let mut gate = self.operation_gate_lock_until(deadline)?;
        loop {
            if gate.active_operations == 0 {
                return if Instant::now() < deadline {
                    Ok(())
                } else {
                    Err(mcp_http_operation_drain_timeout(0))
                };
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(mcp_http_operation_drain_timeout(gate.active_operations));
            }
            let (next, wait) = self
                .0
                .operations_idle
                .wait_timeout(gate, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            gate = next;
            if wait.timed_out() && gate.active_operations != 0 {
                return Err(mcp_http_operation_drain_timeout(gate.active_operations));
            }
        }
    }

    fn lifecycle_residual_until(
        &self,
        deadline: Instant,
    ) -> Result<(usize, usize, usize), EvaError> {
        let gate = self.operation_gate_lock_until(deadline)?;
        let active_operations = gate.active_operations;
        drop(gate);
        let lifecycle = self.lock_until(deadline)?;
        Ok((
            active_operations,
            lifecycle.credential_leases.len(),
            lifecycle.credential_releases.len(),
        ))
    }

    fn operation_gate_lock_until(
        &self,
        deadline: Instant,
    ) -> Result<MutexGuard<'_, McpHttpOperationGate>, EvaError> {
        loop {
            if Instant::now() >= deadline {
                return Err(mcp_http_operation_gate_drain_timeout());
            }
            match self.0.operation_gate.try_lock() {
                Ok(gate) => {
                    if Instant::now() >= deadline {
                        drop(gate);
                        return Err(mcp_http_operation_gate_drain_timeout());
                    }
                    return Ok(gate);
                }
                Err(TryLockError::WouldBlock) => thread::yield_now(),
                Err(TryLockError::Poisoned(_)) => {
                    return Err(
                        EvaError::internal("MCP HTTP operation gate lock is poisoned")
                            .with_provider_code("mcp_http_operation_gate_lock_poisoned"),
                    );
                }
            }
        }
    }

    fn lock_until(
        &self,
        deadline: Instant,
    ) -> Result<MutexGuard<'_, McpHttpLifecycleState>, EvaError> {
        loop {
            if Instant::now() >= deadline {
                return Err(mcp_http_operation_drain_timeout(0));
            }
            match self.0.state.try_lock() {
                Ok(lifecycle) => {
                    if Instant::now() >= deadline {
                        drop(lifecycle);
                        return Err(mcp_http_operation_drain_timeout(0));
                    }
                    return Ok(lifecycle);
                }
                Err(TryLockError::WouldBlock) => thread::yield_now(),
                Err(TryLockError::Poisoned(_)) => {
                    return Err(EvaError::internal(
                        "MCP Streamable HTTP lifecycle registry lock is poisoned",
                    )
                    .with_provider_code("mcp_http_registry_lock_poisoned"));
                }
            }
        }
    }

    pub(crate) fn try_lock(&self) -> Result<MutexGuard<'_, McpHttpLifecycleState>, EvaError> {
        match self.0.state.try_lock() {
            Ok(registry) => Ok(registry),
            Err(TryLockError::WouldBlock) => Err(EvaError::unavailable(
                "MCP Streamable HTTP lifecycle registry is busy",
            )
            .with_provider_code("mcp_http_registry_busy")
            .with_retryable(true)),
            Err(TryLockError::Poisoned(_)) => Err(EvaError::internal(
                "MCP Streamable HTTP lifecycle registry lock is poisoned",
            )
            .with_provider_code("mcp_http_registry_lock_poisoned")),
        }
    }
}

impl fmt::Debug for McpHttpLifecycleHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("McpHttpLifecycleHandle([DAEMON_OWNED])")
    }
}

impl PartialEq for McpHttpLifecycleHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for McpHttpLifecycleHandle {}

/// 汇总一次提供者执行在准入阶段所需的不可变事实和清单限额。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderExecutionRequest {
    /// 记录 `request_id` 字段对应的值。
    pub request_id: RequestId,
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `capability` 字段对应的值。
    pub capability: CapabilityName,
    /// 记录 `transport` 字段对应的值。
    pub transport: AdapterTransport,
    /// 记录 `manifest_digest` 字段对应的值。
    pub manifest_digest: String,
    /// 记录 `start_command` 字段对应的值。
    pub start_command: String,
    /// Canonical provider restart, run-as, and vault-reference declaration.
    pub provider: ProviderConfig,
    /// 限制该适配器同时处于运行状态的快照数量；`None` 表示不设此项限制。
    pub max_concurrency: Option<usize>,
    /// 定义按适配器隔离的固定窗口请求上限。
    pub rate_limit: Option<AdapterRateLimit>,
    /// 定义连续失败阈值和从开启态进入半开探测的恢复窗口。
    pub circuit_breaker: Option<AdapterCircuitBreaker>,
    /// 提供拒绝后建议的重试退避，仅进入错误上下文与进程快照，不负责主动重试。
    pub retry_backoff_ms: Option<u64>,
}

/// 表示已通过所有准入检查并写入运行中快照的执行槽。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderExecutionSlot {
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `provider_process_id` 字段对应的值。
    pub provider_process_id: String,
    /// 记录 `request_id` 字段对应的值。
    pub request_id: RequestId,
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// Durable admission identity; absent for supervisors without a durable admission table.
    pub admission_reservation_id: Option<String>,
    /// 标记该槽是否是熔断恢复窗口后的唯一半开探测。
    pub half_open_probe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderAdmissionLease {
    table: FileSystemProviderAdmissionTable,
    adapter_id: AdapterId,
    reservation_id: String,
    session_id: String,
}

impl ProviderAdmissionLease {
    pub(crate) fn renew_at(&self, now_ms: u128) -> Result<(), EvaError> {
        self.table.renew(
            &self.adapter_id,
            &self.reservation_id,
            &self.session_id,
            now_ms,
            eva_storage::DEFAULT_RESERVATION_TTL_MS,
        )?;
        Ok(())
    }
}

/// 绑定到单一会话、提供者、请求和能力的短生命周期凭据作用域。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCredentialScope {
    /// 记录 `session_id` 字段对应的值。
    pub session_id: String,
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `request_id` 字段对应的值。
    pub request_id: RequestId,
    /// 记录 `capability` 字段对应的值。
    pub capability: CapabilityName,
    /// 保存可审计的确定性摘要；实际注入令牌由摘要和会话标识临时派生。
    pub token_digest: String,
}

/// 表示 `ProviderExecutionOutcome` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderExecutionOutcome {
    /// 记录 `health` 字段对应的值。
    pub health: String,
    /// 记录 `last_error` 字段对应的值。
    pub last_error: Option<String>,
}

/// 约定提供者执行槽从准入到完成的成对生命周期接口。
pub trait ProviderSupervisor {
    /// 原子地完成准入检查并创建运行中快照；失败不得留下活动槽。
    fn acquire(
        &mut self,
        request: ProviderExecutionRequest,
    ) -> Result<ProviderExecutionSlot, EvaError>;
    /// 释放指定槽、记录终态并推进熔断器；同一槽只能完成一次。
    fn complete(
        &mut self,
        slot: &ProviderExecutionSlot,
        outcome: ProviderExecutionOutcome,
    ) -> Result<ProviderProcessSnapshot, EvaError>;
    /// Extend a durable admission lease. Implementations fail closed when ownership is stale.
    fn renew_admission(
        &mut self,
        _slot: &ProviderExecutionSlot,
        _now_ms: u128,
    ) -> Result<(), EvaError> {
        Err(EvaError::unsupported(
            "provider supervisor does not support durable admission renewal",
        ))
    }
    /// Attach the real OS identity immediately after a transport spawn.
    /// Implementations that do not persist process identities fail closed.
    fn register_process_identity(
        &mut self,
        _slot: &ProviderExecutionSlot,
        _identity: &ProcessIdentity,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        Err(EvaError::unsupported(
            "provider supervisor does not support OS process registration",
        ))
    }
    /// Persist a consumed restart attempt and its next due time.
    fn schedule_restart(
        &mut self,
        _slot: &ProviderExecutionSlot,
        _attempt: u32,
        _due_at_ms: u128,
        _reason: &str,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        Err(EvaError::unsupported(
            "provider supervisor does not support durable restart scheduling",
        ))
    }
    /// Move a due durable restart into its pre-spawn state.
    fn prepare_restart(
        &mut self,
        _slot: &ProviderExecutionSlot,
        _now_ms: u128,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        Err(EvaError::unsupported(
            "provider supervisor does not support durable restart preparation",
        ))
    }
    /// Persist terminal budget exhaustion.
    fn exhaust_restart(
        &mut self,
        _slot: &ProviderExecutionSlot,
        _reason: &str,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        Err(EvaError::unsupported(
            "provider supervisor does not support durable restart exhaustion",
        ))
    }
    /// Persist a terminal failure that must not consume restart budget.
    fn fail_restart(
        &mut self,
        _slot: &ProviderExecutionSlot,
        _reason: &str,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        Err(EvaError::unsupported(
            "provider supervisor does not support terminal restart failure",
        ))
    }
    /// Read the authoritative durable snapshot for a slot.
    fn snapshot(&self, _slot: &ProviderExecutionSlot) -> Result<ProviderProcessSnapshot, EvaError> {
        Err(EvaError::unsupported(
            "provider supervisor does not expose durable restart state",
        ))
    }
    /// 执行 `processes` 对应的处理逻辑。
    fn processes(&self) -> Result<Vec<ProviderProcessSnapshot>, EvaError>;

    /// Close admission and retire active provider slots before daemon shutdown.
    /// Implementations that do not own process boundaries fail closed rather
    /// than pretending that a drain completed.
    fn drain(&mut self, _options: ProviderDrainOptions) -> Result<ProviderDrainReport, EvaError> {
        Err(EvaError::unsupported(
            "provider supervisor does not support bounded drain",
        ))
    }
}

/// 在调用线程内维护准入状态，并可镜像快照到持久化进程表的监督器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InMemoryProviderSupervisor {
    /// 作为本实例判断活动槽的权威内存进程表。
    table: InMemoryProviderProcessTable,
    /// 可选的持久化镜像；写入失败会阻止报告准入或完成成功。
    durable_table: Option<FileSystemProviderProcessTable>,
    admission_table: Option<FileSystemProviderAdmissionTable>,
    /// 按适配器隔离的固定窗口计数器。
    rate_windows: BTreeMap<AdapterId, ProviderRateWindow>,
    /// 按适配器隔离的连续失败、开启时间和半开探测状态。
    circuit_states: BTreeMap<AdapterId, ProviderCircuitState>,
    /// Shared lifecycle authority for every Streamable HTTP session admitted
    /// by this supervisor generation.
    mcp_http_lifecycle: McpHttpLifecycleHandle,
    /// One-way admission gate. Once set, no new provider execution may start.
    draining: bool,
    /// Cached successful report makes repeated shutdown requests idempotent.
    drain_report: Option<ProviderDrainReport>,
    /// A failed drain is terminal for this supervisor generation. Retrying
    /// after a partial cleanup could mistake a released snapshot for a fully
    /// released admission reservation.
    drain_error: Option<EvaError>,
}

/// 表示 `ProviderRateWindow` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderRateWindow {
    /// 记录 `started_at_ms` 字段对应的值。
    started_at_ms: u128,
    /// 记录 `count` 字段对应的值。
    count: u32,
}

/// 表示 `ProviderCircuitState` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ProviderCircuitState {
    /// 记录 `failure_count` 字段对应的值。
    failure_count: u32,
    /// 记录 `opened_at_ms` 字段对应的值。
    opened_at_ms: Option<u128>,
    /// 记录 `half_open_probe_active` 字段对应的值。
    half_open_probe_active: bool,
    /// 记录 `failure_threshold` 字段对应的值。
    failure_threshold: u32,
}

impl ProviderExecutionRequest {
    /// 根据输入构造当前类型，作为 `from_handle` 的标准入口。
    pub fn from_handle(handle: &AdapterHandle, invocation: &AdapterInvocation) -> Self {
        Self {
            request_id: invocation.request_id.clone(),
            adapter_id: handle.id.clone(),
            capability: invocation.capability.clone(),
            transport: handle.transport,
            manifest_digest: manifest_digest(handle),
            start_command: start_command(handle),
            provider: handle.provider.clone(),
            max_concurrency: handle.max_concurrency,
            rate_limit: handle.rate_limit,
            circuit_breaker: handle.circuit_breaker,
            retry_backoff_ms: None,
        }
    }

    /// 设置 `retry_backoff_ms` 并返回更新后的实例。
    pub fn with_retry_backoff_ms(mut self, retry_backoff_ms: Option<u64>) -> Self {
        self.retry_backoff_ms = retry_backoff_ms;
        self
    }
}

impl ProviderCredentialScope {
    /// 执行 `new_for_session` 对应的处理逻辑。
    pub fn new_for_session(
        session_id: impl Into<String>,
        adapter_id: AdapterId,
        request_id: RequestId,
        capability: CapabilityName,
    ) -> Self {
        let session_id = session_id.into();
        let token_digest =
            credential_token_digest(&session_id, &adapter_id, &request_id, &capability);
        Self {
            session_id,
            adapter_id,
            request_id,
            capability,
            token_digest,
        }
    }

    /// 根据输入构造当前类型，作为 `from_slot` 的标准入口。
    pub fn from_slot(slot: &ProviderExecutionSlot, capability: CapabilityName) -> Self {
        Self::new_for_session(
            slot.session_id.clone(),
            slot.adapter_id.clone(),
            slot.request_id.clone(),
            capability,
        )
    }

    /// 校验作用域是否精确绑定当前提供者、请求和能力，阻止跨请求或跨提供者复用。
    pub fn ensure_matches(
        &self,
        adapter_id: &AdapterId,
        request_id: &RequestId,
        capability: &CapabilityName,
    ) -> Result<(), EvaError> {
        if &self.adapter_id != adapter_id {
            return Err(EvaError::permission_denied(
                "provider credential session cannot be reused across providers",
            )
            .with_context("session_id", &self.session_id)
            .with_context("session_provider", self.adapter_id.as_str())
            .with_context("requested_provider", adapter_id.as_str()));
        }
        if &self.request_id != request_id || &self.capability != capability {
            return Err(EvaError::permission_denied(
                "provider credential session cannot be reused across requests",
            )
            .with_context("session_id", &self.session_id)
            .with_context("session_request", self.request_id.as_str())
            .with_context("requested_request", request_id.as_str()));
        }
        Ok(())
    }

    /// 执行 `audit_entries` 对应的处理逻辑。
    pub fn audit_entries(&self) -> Vec<String> {
        vec![
            "credential.scope:provider_session".to_owned(),
            format!("credential.session:{}", self.session_id),
            format!("credential.session_digest:{}", self.token_digest),
            "credential.session_token:redacted".to_owned(),
        ]
    }

    /// 执行 `apply_env` 对应的处理逻辑。
    pub(crate) fn apply_env(&self, env: &mut BTreeMap<String, String>) {
        env.insert(PROVIDER_SESSION_ID_ENV.to_owned(), self.session_id.clone());
        env.insert(PROVIDER_SESSION_TOKEN_ENV.to_owned(), self.session_token());
    }

    /// 执行 `apply_headers` 对应的处理逻辑。
    pub(crate) fn apply_headers(&self, headers: &mut BTreeMap<String, String>) {
        headers.insert(
            PROVIDER_SESSION_ID_HEADER.to_owned(),
            self.session_id.clone(),
        );
        headers.insert(
            PROVIDER_SESSION_TOKEN_HEADER.to_owned(),
            self.session_token(),
        );
    }

    /// 执行 `redaction_values` 对应的处理逻辑。
    pub(crate) fn redaction_values(&self) -> Vec<String> {
        vec![self.session_token()]
    }

    /// 执行 `session_token` 对应的处理逻辑。
    fn session_token(&self) -> String {
        format!(
            "eva-provider-session:{}:{}",
            self.session_id, self.token_digest
        )
    }
}

impl ProviderExecutionOutcome {
    /// 执行 `completed` 对应的处理逻辑。
    pub fn completed(status: &str) -> Self {
        Self {
            health: if status == "completed" {
                "completed".to_owned()
            } else {
                "failed".to_owned()
            },
            last_error: if status == "completed" {
                None
            } else {
                Some(format!("adapter returned status {status}"))
            },
        }
    }

    /// 执行 `failed` 对应的处理逻辑。
    pub fn failed(error: &EvaError) -> Self {
        Self {
            health: "failed".to_owned(),
            last_error: Some(format!("{}: {}", error.kind().as_str(), error.message())),
        }
    }
}

/// 校验可选凭据作用域；需要凭据的传输在缺失作用域时必须在 I/O 前失败。
pub(crate) fn validate_credential_scope_for_provider<'a>(
    scope: Option<&'a ProviderCredentialScope>,
    adapter_id: &AdapterId,
    request_id: &RequestId,
    capability: &CapabilityName,
    required: bool,
) -> Result<Option<&'a ProviderCredentialScope>, EvaError> {
    match scope {
        Some(scope) => {
            scope.ensure_matches(adapter_id, request_id, capability)?;
            Ok(Some(scope))
        }
        None if required => Err(EvaError::permission_denied(
            "provider credential session scope is required",
        )
        .with_context("adapter_id", adapter_id.as_str())
        .with_context("request_id", request_id.as_str())),
        None => Ok(None),
    }
}

/// 执行 `redact_provider_session_tokens` 对应的处理逻辑。
pub(crate) fn redact_provider_session_tokens(value: &str) -> String {
    let mut redacted = value.to_owned();
    while let Some(start) = redacted.find("eva-provider-session:") {
        let end = redacted[start..]
            .char_indices()
            .find_map(|(offset, ch)| {
                if offset > 0 && (ch.is_whitespace() || matches!(ch, '"' | '\'' | '\\' | '<' | '>'))
                {
                    Some(start + offset)
                } else {
                    None
                }
            })
            .unwrap_or(redacted.len());
        redacted.replace_range(start..end, "[REDACTED]");
    }
    redacted
}

impl Default for InMemoryProviderSupervisor {
    /// 创建仅使用内存进程表且无历史限流状态的监督器。
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryProviderSupervisor {
    /// 创建并初始化当前类型的实例。
    pub fn new() -> Self {
        Self {
            table: InMemoryProviderProcessTable::new(),
            durable_table: None,
            admission_table: None,
            rate_windows: BTreeMap::new(),
            circuit_states: BTreeMap::new(),
            mcp_http_lifecycle: McpHttpLifecycleHandle::new(),
            draining: false,
            drain_report: None,
            drain_error: None,
        }
    }

    /// 设置 `process_table` 并返回更新后的实例。
    pub fn with_process_table(durable_table: FileSystemProviderProcessTable) -> Self {
        let admission_root = durable_table.root_path().join("admission");
        let admission_table = FileSystemProviderAdmissionTable::new(admission_root).ok();
        Self {
            table: InMemoryProviderProcessTable::new(),
            durable_table: Some(durable_table),
            admission_table,
            rate_windows: BTreeMap::new(),
            circuit_states: BTreeMap::new(),
            mcp_http_lifecycle: McpHttpLifecycleHandle::new(),
            draining: false,
            drain_report: None,
            drain_error: None,
        }
    }

    pub(crate) fn admission_lease(
        &self,
        slot: &ProviderExecutionSlot,
    ) -> Result<Option<ProviderAdmissionLease>, EvaError> {
        let Some(table) = self.admission_table.clone() else {
            return Ok(None);
        };
        let reservation_id = slot.admission_reservation_id.clone().ok_or_else(|| {
            EvaError::conflict("provider execution slot lacks admission identity")
        })?;
        Ok(Some(ProviderAdmissionLease {
            table,
            adapter_id: slot.adapter_id.clone(),
            reservation_id,
            session_id: slot.session_id.clone(),
        }))
    }

    /// 执行 `active_for_adapter` 对应的处理逻辑。
    pub fn active_for_adapter(
        &self,
        adapter_id: &AdapterId,
    ) -> Result<Vec<ProviderProcessSnapshot>, EvaError> {
        if let Some(table) = &self.durable_table {
            return Ok(table
                .list()?
                .into_iter()
                .filter(|snapshot| snapshot.active && &snapshot.adapter_id == adapter_id)
                .collect());
        }
        self.table.active_for_adapter(adapter_id)
    }

    /// Returns whether this supervisor has closed provider admission.
    pub fn is_draining(&self) -> bool {
        self.draining
    }

    pub(crate) fn mcp_http_lifecycle(&self) -> McpHttpLifecycleHandle {
        self.mcp_http_lifecycle.clone()
    }

    /// Close admission, wait for in-flight providers, and force-clean any
    /// boundary that remains at the absolute deadline. The gate is one-way;
    /// a successful report is cached so repeated daemon shutdowns cannot
    /// accidentally reopen admission or release a successor reservation.
    pub fn drain(
        &mut self,
        options: ProviderDrainOptions,
    ) -> Result<ProviderDrainReport, EvaError> {
        if let Some(previous) = &self.drain_report {
            let mut repeated = previous.clone();
            repeated.already_drained = true;
            return Ok(repeated);
        }
        if let Some(previous) = &self.drain_error {
            return Err(previous.clone());
        }
        self.draining = true;
        let deadline = Instant::now()
            .checked_add(options.total_timeout())
            .ok_or_else(|| {
                EvaError::invalid_argument("provider drain deadline is outside the clock range")
            })?;
        if deadline.saturating_duration_since(Instant::now()).is_zero() {
            return Err(
                EvaError::timeout("provider drain deadline elapsed before MCP cleanup")
                    .with_context("cleanup_blocked", "true"),
            );
        }
        // MCP cleanup runs before process-slot retirement so a failed DELETE
        // can be retried without confusing a released admission reservation
        // with a complete daemon drain.
        let mcp_drain = self
            .mcp_http_lifecycle
            .drain_all_until(deadline)
            .map_err(|error| error.with_context("provider_drain_phase", "mcp_http_cleanup"))?;
        if !mcp_drain.complete
            || mcp_drain.sessions_after != 0
            || mcp_drain.readers_after != 0
            || mcp_drain.cleanup_pending_after != 0
        {
            return Err(EvaError::unavailable(
                "MCP Streamable HTTP drain left owned lifecycle state behind",
            )
            .with_provider_code("mcp_http_registry_drain_incomplete")
            .with_context("sessions_after", mcp_drain.sessions_after.to_string())
            .with_context("readers_after", mcp_drain.readers_after.to_string())
            .with_context(
                "cleanup_pending_after",
                mcp_drain.cleanup_pending_after.to_string(),
            ));
        }
        let result = self
            .reconcile_pending_admission_releases()
            .and_then(|_| self.drain_once(options, deadline, &mcp_drain));
        match result {
            Ok(report) => {
                self.drain_report = Some(report.clone());
                Ok(report)
            }
            Err(error) => {
                self.drain_error = Some(error.clone());
                Err(error)
            }
        }
    }

    fn drain_once(
        &mut self,
        options: ProviderDrainOptions,
        deadline: Instant,
        mcp_drain: &eva_mcp::McpHttpDrainReport,
    ) -> Result<ProviderDrainReport, EvaError> {
        let backend = OsProcessBackend::new();
        let mut terminated_provider_count = 0usize;
        let mut forced_provider_count = 0usize;
        let mut missing_identity_count = 0usize;
        let mut audit = vec![
            "provider.lifecycle:admission_closed".to_owned(),
            format!(
                "provider.lifecycle:drain_timeout_ms:{}",
                options.total_timeout().as_millis()
            ),
            "mcp.http_registry:admission_closed".to_owned(),
            format!(
                "mcp.http_registry:drained:sessions_before={}:readers_before={}:sessions_after=0:readers_after=0",
                mcp_drain.sessions_before, mcp_drain.readers_before
            ),
        ];

        loop {
            let active = self
                .processes()?
                .into_iter()
                .filter(|snapshot| snapshot.active)
                .collect::<Vec<_>>();
            if active.is_empty() {
                if Instant::now() >= deadline {
                    return Err(EvaError::timeout(
                        "provider supervisor drain deadline elapsed before completion",
                    )
                    .with_context("active_provider_count", "0")
                    .with_context(
                        "terminated_provider_count",
                        terminated_provider_count.to_string(),
                    )
                    .with_context("cleanup_blocked", "false"));
                }
                let report = ProviderDrainReport {
                    already_drained: false,
                    phase: "drained".to_owned(),
                    active_provider_count: 0,
                    terminated_provider_count,
                    forced_provider_count,
                    missing_identity_count,
                    mcp_http_sessions_before: mcp_drain.sessions_before,
                    mcp_http_readers_before: mcp_drain.readers_before,
                    mcp_http_sessions_after: mcp_drain.sessions_after,
                    mcp_http_readers_after: mcp_drain.readers_after,
                    mcp_http_cleanup_pending_after: mcp_drain.cleanup_pending_after,
                    audit: {
                        audit.push("provider.lifecycle:drained".to_owned());
                        audit
                    },
                };
                return Ok(report);
            }

            // The supervisor is single-threaded behind AdapterRuntime's
            // RefCell. Only a committed v3 slot that is still in the initial
            // or restart-starting state can be the known pre-spawn window.
            // Legacy v1/v2 records retain `restart_state=unconfigured` and
            // must remain fail-closed when their OS identity is missing.
            let mut retired_unregistered = false;
            for snapshot in &active {
                let unregistered = !snapshot.has_process_identity()
                    && snapshot.attempt == 0
                    && snapshot.record_version.0 > 0
                    && snapshot.restart_state != "unconfigured"
                    && !snapshot
                        .audit
                        .iter()
                        .any(|entry| entry == "provider.process:registered");
                if unregistered
                    && self.retire_snapshot_after_drain(
                        snapshot,
                        "provider supervisor drain closed an unregistered slot",
                    )?
                {
                    retired_unregistered = true;
                    missing_identity_count += 1;
                    audit.push(format!(
                        "provider.lifecycle:retired_unregistered:{}",
                        snapshot.session_id
                    ));
                }
            }
            if retired_unregistered {
                continue;
            }

            let remaining_budget = deadline.saturating_duration_since(Instant::now());
            if remaining_budget.is_zero() {
                let remaining = active.len();
                return Err(EvaError::timeout(
                    "provider supervisor drain left active providers behind",
                )
                .with_context("active_provider_count", remaining.to_string())
                .with_context(
                    "terminated_provider_count",
                    terminated_provider_count.to_string(),
                )
                .with_context("cleanup_blocked", "true"));
            }

            // Start graceful termination while there is still budget. The
            // previous implementation waited until the deadline and passed
            // zero, which skipped the cooperative window entirely. Divide the
            // remaining budget across active boundaries so one provider cannot
            // consume the entire shutdown window before its siblings are
            // observed.
            let per_provider_budget = remaining_budget
                .checked_div(active.len() as u32)
                .unwrap_or(remaining_budget);
            let graceful_timeout = per_provider_budget / 2;
            let force_timeout = per_provider_budget.saturating_sub(graceful_timeout);
            let mut cleanup_blocked = false;
            for snapshot in &active {
                if !snapshot.has_process_identity() {
                    missing_identity_count += 1;
                    cleanup_blocked = true;
                    audit.push(format!(
                        "provider.lifecycle:missing_identity:{}",
                        snapshot.session_id
                    ));
                    continue;
                }
                match backend.terminate_snapshot_with_force_timeout(
                    snapshot,
                    graceful_timeout,
                    force_timeout,
                ) {
                    Ok(termination)
                        if matches!(
                            termination.outcome,
                            ProcessTerminationOutcome::AlreadyExited
                                | ProcessTerminationOutcome::Graceful
                                | ProcessTerminationOutcome::Forced
                        ) =>
                    {
                        terminated_provider_count += 1;
                        if termination.outcome == ProcessTerminationOutcome::Forced {
                            forced_provider_count += 1;
                        }
                        audit.extend(termination.audit_entries());
                        if self.retire_snapshot_after_drain(
                            snapshot,
                            "provider supervisor graceful drain",
                        )? {
                            audit.push(format!(
                                "provider.lifecycle:retired:{}",
                                snapshot.session_id
                            ));
                        }
                    }
                    Ok(termination) => {
                        cleanup_blocked = true;
                        audit.extend(termination.audit_entries());
                        audit.push(format!(
                            "provider.lifecycle:cleanup_blocked:{}",
                            snapshot.session_id
                        ));
                    }
                    Err(error) => {
                        cleanup_blocked = true;
                        audit.push(format!(
                            "provider.lifecycle:cleanup_error:{}",
                            sanitize_drain_value(error.message())
                        ));
                    }
                }
            }

            let remaining = self
                .processes()?
                .into_iter()
                .filter(|snapshot| snapshot.active)
                .count();
            if remaining == 0 {
                if Instant::now() >= deadline {
                    return Err(EvaError::timeout(
                        "provider supervisor drain deadline elapsed before completion",
                    )
                    .with_context("active_provider_count", "0")
                    .with_context(
                        "terminated_provider_count",
                        terminated_provider_count.to_string(),
                    )
                    .with_context("cleanup_blocked", "false"));
                }
                let report = ProviderDrainReport {
                    already_drained: false,
                    phase: "drained".to_owned(),
                    active_provider_count: 0,
                    terminated_provider_count,
                    forced_provider_count,
                    missing_identity_count,
                    mcp_http_sessions_before: mcp_drain.sessions_before,
                    mcp_http_readers_before: mcp_drain.readers_before,
                    mcp_http_sessions_after: mcp_drain.sessions_after,
                    mcp_http_readers_after: mcp_drain.readers_after,
                    mcp_http_cleanup_pending_after: mcp_drain.cleanup_pending_after,
                    audit: {
                        audit.push("provider.lifecycle:drained_after_graceful".to_owned());
                        audit
                    },
                };
                return Ok(report);
            }
            if Instant::now() >= deadline {
                return Err(EvaError::timeout(
                    "provider supervisor drain left active providers behind",
                )
                .with_context("active_provider_count", remaining.to_string())
                .with_context(
                    "terminated_provider_count",
                    terminated_provider_count.to_string(),
                )
                .with_context("cleanup_blocked", cleanup_blocked.to_string()));
            }
            let remaining_budget = deadline.saturating_duration_since(Instant::now());
            thread::sleep(options.poll_interval().min(remaining_budget));
        }
    }

    fn retire_snapshot_after_drain(
        &mut self,
        snapshot: &ProviderProcessSnapshot,
        reason: &str,
    ) -> Result<bool, EvaError> {
        let mut current = match self.read_process(&snapshot.session_id) {
            Ok(current) => current,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        if !current.active {
            return Ok(false);
        }
        let admission_release = self.admission_release_for_snapshot(&current)?;
        if let Some((_, reservation_id)) = &admission_release {
            current.audit.push(format!(
                "{PROVIDER_ADMISSION_RELEASE_PENDING_AUDIT_PREFIX}{reservation_id}"
            ));
        }
        // A completion racing with drain is allowed to win. The process-table
        // CAS below then either commits this terminal state or reports a
        // version conflict, which the next list pass re-evaluates.
        current.release("interrupted", Some(format!("provider shutdown: {reason}")))?;
        current
            .audit
            .push("provider.lifecycle:shutdown_retired".to_owned());
        let committed = match self.upsert_process(current) {
            Ok(committed) => committed,
            Err(error) if error.kind() == ErrorKind::Conflict => return Ok(false),
            Err(error) => return Err(error),
        };
        // Release only the reservation identity persisted with this provider
        // incarnation. A successor may reuse the session ID, but cannot reuse
        // the old reservation ID without violating the admission fence.
        if let Some((table, reservation_id)) = admission_release {
            match table.release_owned(
                &committed.adapter_id,
                &reservation_id,
                &committed.session_id,
            ) {
                Ok(()) => {
                    self.mark_admission_release_resolved(&committed, &reservation_id)?;
                }
                Err(error) if error.kind() == ErrorKind::Conflict => {
                    // The reservation may have expired between ownership
                    // proof and release. Reconcile immediately; a successor
                    // with the same session but a different reservation ID is
                    // preserved by the reconciliation fence.
                    self.reconcile_pending_admission_releases()?;
                    let latest = self.read_process(&committed.session_id)?;
                    if pending_release_id(&latest).is_some_and(|id| id == reservation_id) {
                        return Err(error
                            .with_context("session_id", &committed.session_id)
                            .with_context("reservation_id", reservation_id));
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(true)
    }

    fn admission_release_for_snapshot(
        &self,
        snapshot: &ProviderProcessSnapshot,
    ) -> Result<Option<(FileSystemProviderAdmissionTable, String)>, EvaError> {
        let Some(table) = self.admission_table.clone() else {
            return Ok(None);
        };
        let expected_reservation_id = reservation_id_from_audit(snapshot).ok_or_else(|| {
            EvaError::conflict("provider drain lacks durable admission reservation identity")
                .with_context("session_id", &snapshot.session_id)
        })?;
        let state = table.snapshot(&snapshot.adapter_id, now_ms())?;
        let same_session = state
            .reservations
            .iter()
            .filter(|reservation| reservation.session_id == snapshot.session_id)
            .collect::<Vec<_>>();
        match same_session.as_slice() {
            [reservation] if reservation.reservation_id == expected_reservation_id => {
                Ok(Some((table, expected_reservation_id)))
            }
            // An expired reservation is already absent after `snapshot`'
            // cleanup. There is nothing left to release; a successor would
            // have appeared in this same list and is handled as a conflict.
            [] => Ok(None),
            _ => Err(
                EvaError::conflict("provider drain found ambiguous admission reservations")
                    .with_context("session_id", &snapshot.session_id),
            ),
        }
    }

    fn mark_admission_release_resolved(
        &mut self,
        snapshot: &ProviderProcessSnapshot,
        reservation_id: &str,
    ) -> Result<(), EvaError> {
        let mut current = self.read_process(&snapshot.session_id)?;
        if !current.audit.iter().any(|entry| {
            entry == &format!("{PROVIDER_ADMISSION_RELEASE_RESOLVED_AUDIT_PREFIX}{reservation_id}")
        }) {
            current.audit.push(format!(
                "{PROVIDER_ADMISSION_RELEASE_RESOLVED_AUDIT_PREFIX}{reservation_id}"
            ));
            match self.upsert_process(current) {
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::Conflict => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Replays durable release intents left by a crash or an admission-table
    /// I/O failure. The exact reservation ID is the fence; a successor using
    /// the same session ID is never removed.
    fn reconcile_pending_admission_releases(&mut self) -> Result<(), EvaError> {
        let Some(table) = self.admission_table.clone() else {
            return Ok(());
        };
        let snapshots = self.processes()?;
        for snapshot in snapshots {
            for reservation_id in pending_release_ids(&snapshot) {
                let state = table.snapshot(&snapshot.adapter_id, now_ms())?;
                let exact = state.reservations.iter().any(|reservation| {
                    reservation.reservation_id == reservation_id
                        && reservation.session_id == snapshot.session_id
                });
                let successor = state.reservations.iter().any(|reservation| {
                    reservation.session_id == snapshot.session_id
                        && reservation.reservation_id != reservation_id
                });
                if exact {
                    table.release_owned(
                        &snapshot.adapter_id,
                        &reservation_id,
                        &snapshot.session_id,
                    )?;
                }
                // A successor with the same session ID is intentionally
                // preserved: the old reservation is already absent, so
                // resolving this intent is safe and avoids retrying forever
                // after an expiry/restart.
                let mut current = self.read_process(&snapshot.session_id)?;
                if successor
                    && !current.audit.iter().any(|entry| {
                        entry == &format!("provider.admission:successor_preserved:{reservation_id}")
                    })
                {
                    current.audit.push(format!(
                        "provider.admission:successor_preserved:{reservation_id}"
                    ));
                    self.upsert_process(current)?;
                }
                self.mark_admission_release_resolved(&snapshot, &reservation_id)?;
            }
        }
        Ok(())
    }
}

impl ProviderSupervisor for InMemoryProviderSupervisor {
    /// 按熔断、并发、速率的顺序完成准入，全部通过后才写入运行中快照。
    fn acquire(
        &mut self,
        request: ProviderExecutionRequest,
    ) -> Result<ProviderExecutionSlot, EvaError> {
        self.reconcile_pending_admission_releases()?;
        if self.draining {
            return Err(admission_error(
                &request,
                "provider supervisor is draining",
                "provider_draining",
                None,
            ));
        }
        let now = now_ms();
        let session_id = session_id(&request.request_id, &request.adapter_id);
        let provider_process_id = provider_process_id(&request.request_id, &request.adapter_id);
        match self.read_process(&session_id) {
            Ok(mut existing) => {
                ensure_request_matches_snapshot(&request, &existing)?;
                existing.prepare_for_restart(now)?;
                let admission_reservation_id = if let Some(table) = &self.admission_table {
                    let matches = table
                        .snapshot(&existing.adapter_id, now)?
                        .reservations
                        .into_iter()
                        .filter(|reservation| reservation.session_id == existing.session_id)
                        .collect::<Vec<_>>();
                    match matches.as_slice() {
                        [reservation] => Some(reservation.reservation_id.clone()),
                        [] => {
                            return Err(EvaError::conflict(
                                "provider restart lacks an active admission reservation",
                            ))
                        }
                        _ => {
                            return Err(EvaError::conflict(
                                "provider restart has conflicting admission reservations",
                            ))
                        }
                    }
                } else {
                    None
                };
                if let Some(reservation_id) = &admission_reservation_id {
                    existing.audit.push(format!(
                        "{PROVIDER_ADMISSION_RESERVATION_AUDIT_PREFIX}{reservation_id}"
                    ));
                }
                let committed = self.upsert_process(existing)?;
                return Ok(ProviderExecutionSlot {
                    session_id: committed.session_id,
                    provider_process_id: committed.provider_process_id,
                    request_id: committed.request_id,
                    adapter_id: committed.adapter_id,
                    admission_reservation_id,
                    half_open_probe: false,
                });
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        // 熔断检查会占用半开探测权，因此其后任何检查失败都不能产生进程快照。
        let half_open_probe = self.admit_circuit(&request, now)?;
        self.admit_concurrency(&request)?;
        self.admit_rate_limit(&request, now)?;
        let limit_audit = limit_audit_entries(&request, half_open_probe);
        let restart_policy = request.provider.restart.mode.as_str().to_owned();
        let mut snapshot = ProviderProcessSnapshot::running(
            session_id.clone(),
            provider_process_id.clone(),
            request.request_id.clone(),
            request.adapter_id.clone(),
            request.capability,
            request.transport.as_str(),
            request.manifest_digest,
            request.start_command,
            restart_policy,
        );
        snapshot.audit.extend(limit_audit);
        snapshot.retry_backoff_ms = request.retry_backoff_ms;
        snapshot.configure_restart_budget(
            request.provider.restart.max_attempts,
            request.provider.restart.backoff_ms,
        )?;
        let admission_reservation = if let Some(table) = &self.admission_table {
            Some(table.reserve(
                &request.adapter_id,
                request.max_concurrency.unwrap_or(usize::MAX),
                &session_id,
                now,
                eva_storage::DEFAULT_RESERVATION_TTL_MS,
            )?)
        } else {
            None
        };
        let admission_reservation_id = admission_reservation
            .as_ref()
            .map(|reservation| reservation.reservation_id.clone());
        if let Some(reservation_id) = &admission_reservation_id {
            snapshot.audit.push(format!(
                "{PROVIDER_ADMISSION_RESERVATION_AUDIT_PREFIX}{reservation_id}"
            ));
        }
        if let Err(error) = self.upsert_process(snapshot) {
            if let (Some(table), Some(reservation_id)) =
                (&self.admission_table, admission_reservation_id.as_deref())
            {
                let _ = table.release_owned(&request.adapter_id, reservation_id, &session_id);
            }
            return Err(error);
        }
        Ok(ProviderExecutionSlot {
            session_id,
            provider_process_id,
            request_id: request.request_id,
            adapter_id: request.adapter_id,
            admission_reservation_id,
            half_open_probe,
        })
    }

    /// 将运行中快照释放为终态，再用同一结果更新熔断状态并同步持久化镜像。
    fn complete(
        &mut self,
        slot: &ProviderExecutionSlot,
        outcome: ProviderExecutionOutcome,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        let mut snapshot = if let Some(table) = &self.durable_table {
            table.read(&slot.session_id)?
        } else {
            self.table.read(&slot.session_id)?
        };
        if !snapshot.active {
            return Err(EvaError::conflict(
                "provider execution slot has already reached a terminal state",
            )
            .with_context("session_id", &slot.session_id)
            .with_context("health", &snapshot.health));
        }
        snapshot.release(outcome.health, outcome.last_error)?;
        if snapshot.last_error.is_none() && snapshot.health == "completed" {
            snapshot.mark_stable_success()?;
        }
        self.record_circuit_outcome(slot, &mut snapshot);
        let result = self.upsert_process(snapshot);
        if result.is_ok() {
            if let Some(table) = &self.admission_table {
                let reservation_id = slot.admission_reservation_id.as_deref().ok_or_else(|| {
                    EvaError::conflict("provider execution slot lacks admission identity")
                })?;
                table.release_owned(&slot.adapter_id, reservation_id, &slot.session_id)?;
            }
        }
        result
    }

    fn renew_admission(
        &mut self,
        slot: &ProviderExecutionSlot,
        now_ms: u128,
    ) -> Result<(), EvaError> {
        let table = self.admission_table.as_ref().ok_or_else(|| {
            EvaError::unsupported("provider supervisor has no durable admission table")
        })?;
        let reservation_id = slot.admission_reservation_id.as_deref().ok_or_else(|| {
            EvaError::conflict("provider execution slot lacks admission identity")
        })?;
        table.renew(
            &slot.adapter_id,
            reservation_id,
            &slot.session_id,
            now_ms,
            eva_storage::DEFAULT_RESERVATION_TTL_MS,
        )?;
        Ok(())
    }

    fn register_process_identity(
        &mut self,
        slot: &ProviderExecutionSlot,
        identity: &ProcessIdentity,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        InMemoryProviderSupervisor::register_process_identity(self, slot, identity)
    }

    fn schedule_restart(
        &mut self,
        slot: &ProviderExecutionSlot,
        attempt: u32,
        due_at_ms: u128,
        reason: &str,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        if self.draining {
            return Err(EvaError::unavailable(
                "provider supervisor is draining; restart scheduling is closed",
            )
            .with_provider_code("provider_draining")
            .with_context("session_id", &slot.session_id));
        }
        let mut snapshot = self.read_process(&slot.session_id)?;
        ensure_slot_matches_snapshot(slot, &snapshot)?;
        snapshot.mark_restart_pending(attempt, due_at_ms, reason)?;
        self.upsert_process(snapshot)
    }

    fn prepare_restart(
        &mut self,
        slot: &ProviderExecutionSlot,
        now_ms: u128,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        if self.draining {
            return Err(EvaError::unavailable(
                "provider supervisor is draining; restart admission is closed",
            )
            .with_provider_code("provider_draining")
            .with_context("session_id", &slot.session_id));
        }
        let mut snapshot = self.read_process(&slot.session_id)?;
        ensure_slot_matches_snapshot(slot, &snapshot)?;
        snapshot.prepare_for_restart(now_ms)?;
        self.upsert_process(snapshot)
    }

    fn exhaust_restart(
        &mut self,
        slot: &ProviderExecutionSlot,
        reason: &str,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        let mut snapshot = self.read_process(&slot.session_id)?;
        ensure_slot_matches_snapshot(slot, &snapshot)?;
        snapshot.mark_restart_exhausted(reason)?;
        self.upsert_process(snapshot)
    }

    fn fail_restart(
        &mut self,
        slot: &ProviderExecutionSlot,
        reason: &str,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        let mut snapshot = self.read_process(&slot.session_id)?;
        ensure_slot_matches_snapshot(slot, &snapshot)?;
        snapshot.mark_restart_failed(reason)?;
        self.upsert_process(snapshot)
    }

    fn snapshot(&self, slot: &ProviderExecutionSlot) -> Result<ProviderProcessSnapshot, EvaError> {
        let snapshot = self.read_process(&slot.session_id)?;
        ensure_slot_matches_snapshot(slot, &snapshot)?;
        Ok(snapshot)
    }

    /// 执行 `processes` 对应的处理逻辑。
    fn processes(&self) -> Result<Vec<ProviderProcessSnapshot>, EvaError> {
        if let Some(table) = &self.durable_table {
            return table.list();
        }
        self.table.list()
    }

    fn drain(&mut self, options: ProviderDrainOptions) -> Result<ProviderDrainReport, EvaError> {
        InMemoryProviderSupervisor::drain(self, options)
    }
}

impl InMemoryProviderSupervisor {
    /// Attach a real OS identity to an already admitted provider slot.
    ///
    /// The slot is created before transport spawn so admission limits remain
    /// authoritative. This method performs the second, fenced CAS immediately
    /// after spawn; callers must terminate the returned handle if this method
    /// fails, ensuring a failed registration cannot leave an orphan.
    pub(crate) fn register_process_identity(
        &mut self,
        slot: &ProviderExecutionSlot,
        identity: &ProcessIdentity,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        if self.draining {
            return Err(EvaError::unavailable(
                "provider supervisor is draining; process registration is closed",
            )
            .with_provider_code("provider_draining")
            .with_context("session_id", &slot.session_id));
        }
        let mut snapshot = self.read_process(&slot.session_id)?;
        ensure_slot_matches_snapshot(slot, &snapshot)?;
        let restarting = snapshot.restart_state == "starting";
        if (!snapshot.active || snapshot.health != "running") && !restarting {
            return Err(EvaError::conflict(
                "provider process registration requires an active running slot",
            )
            .with_context("session_id", &slot.session_id));
        }
        if snapshot.has_process_identity() {
            return Err(
                EvaError::conflict("provider process slot already has an OS identity")
                    .with_context("session_id", &slot.session_id),
            );
        }
        let process_attempt = snapshot.next_process_attempt();
        identity.stamp_snapshot(&mut snapshot, process_attempt)?;
        if restarting {
            snapshot.mark_restart_running()?;
        }
        snapshot
            .audit
            .push("provider.process:registered".to_owned());
        snapshot
            .audit
            .push(format!("provider.pid:{}", identity.pid));
        snapshot.audit.push(format!(
            "provider.process_boundary:{}",
            if identity.process_group_id.is_some() {
                "unix_group"
            } else {
                "windows_job"
            }
        ));
        self.upsert_process(snapshot)
    }

    fn read_process(&self, session_id: &str) -> Result<ProviderProcessSnapshot, EvaError> {
        if let Some(table) = &self.durable_table {
            table.read(session_id)
        } else {
            self.table.read(session_id)
        }
    }

    /// 执行 `upsert_process` 对应的处理逻辑。
    fn upsert_process(
        &mut self,
        snapshot: ProviderProcessSnapshot,
    ) -> Result<ProviderProcessSnapshot, EvaError> {
        if let Some(table) = &mut self.durable_table {
            // The durable table is authoritative whenever configured; all
            // reads and completion paths use it, so no second mirror CAS can
            // create a partial-commit failure after this write succeeds.
            table.compare_and_set(snapshot)
        } else {
            self.table.compare_and_set(snapshot)
        }
    }

    /// 对当前进程表中的活动快照计数，达到上限时返回可重试的稳定准入错误。
    fn admit_concurrency(&self, request: &ProviderExecutionRequest) -> Result<(), EvaError> {
        let Some(max_concurrency) = request.max_concurrency else {
            return Ok(());
        };
        let active = self.active_for_adapter(&request.adapter_id)?.len();
        if active >= max_concurrency {
            return Err(admission_error(
                request,
                "provider concurrency limit is exhausted",
                "provider_concurrency_limited",
                request.retry_backoff_ms,
            )
            .with_context("active", active.to_string())
            .with_context("max_concurrency", max_concurrency.to_string()));
        }
        Ok(())
    }

    /// 在适配器独立的固定时间窗内计数；被拒绝的请求不会增加窗口计数。
    fn admit_rate_limit(
        &mut self,
        request: &ProviderExecutionRequest,
        now: u128,
    ) -> Result<(), EvaError> {
        let Some(limit) = request.rate_limit else {
            return Ok(());
        };
        let window = self
            .rate_windows
            .entry(request.adapter_id.clone())
            .or_insert(ProviderRateWindow {
                started_at_ms: now,
                count: 0,
            });
        if now.saturating_sub(window.started_at_ms) >= u128::from(limit.window_ms) {
            window.started_at_ms = now;
            window.count = 0;
        }
        if window.count >= limit.max_requests {
            let elapsed = now.saturating_sub(window.started_at_ms);
            let retry_after_ms = u128::from(limit.window_ms)
                .saturating_sub(elapsed)
                .try_into()
                .unwrap_or(u64::MAX);
            return Err(admission_error(
                request,
                "provider rate limit is exhausted",
                "provider_rate_limited",
                Some(retry_after_ms),
            )
            .with_context("rate_limit_max_requests", limit.max_requests.to_string())
            .with_context("rate_limit_window_ms", limit.window_ms.to_string()));
        }
        window.count = window.count.saturating_add(1);
        Ok(())
    }

    /// 拒绝开启态请求，恢复窗口届满后仅允许一个半开探测槽继续。
    fn admit_circuit(
        &mut self,
        request: &ProviderExecutionRequest,
        now: u128,
    ) -> Result<bool, EvaError> {
        let Some(config) = request.circuit_breaker else {
            return Ok(false);
        };
        let state = self
            .circuit_states
            .entry(request.adapter_id.clone())
            .or_default();
        state.failure_threshold = config.failure_threshold;
        let Some(opened_at_ms) = state.opened_at_ms else {
            return Ok(false);
        };
        let elapsed = now.saturating_sub(opened_at_ms);
        if elapsed >= u128::from(config.recovery_window_ms) && !state.half_open_probe_active {
            state.half_open_probe_active = true;
            return Ok(true);
        }
        let retry_after_ms = u128::from(config.recovery_window_ms)
            .saturating_sub(elapsed)
            .try_into()
            .unwrap_or(u64::MAX);
        Err(admission_error(
            request,
            "provider circuit breaker is open",
            "provider_circuit_open",
            Some(retry_after_ms),
        )
        .with_context(
            "circuit_failure_threshold",
            config.failure_threshold.to_string(),
        )
        .with_context(
            "circuit_recovery_window_ms",
            config.recovery_window_ms.to_string(),
        ))
    }

    /// 根据执行终态关闭、累加或重新开启熔断器，并把状态变化写入快照审计。
    fn record_circuit_outcome(
        &mut self,
        slot: &ProviderExecutionSlot,
        snapshot: &mut ProviderProcessSnapshot,
    ) {
        let Some(state) = self.circuit_states.get_mut(&slot.adapter_id) else {
            return;
        };
        if snapshot.health == "completed" {
            state.failure_count = 0;
            state.opened_at_ms = None;
            state.half_open_probe_active = false;
            if slot.half_open_probe {
                snapshot.audit.push("provider.circuit:closed".to_owned());
            }
            return;
        }

        state.failure_count = state.failure_count.saturating_add(1);
        state.half_open_probe_active = false;
        if slot.half_open_probe
            || (state.failure_threshold > 0 && state.failure_count >= state.failure_threshold)
        {
            state.opened_at_ms = Some(now_ms());
            snapshot.health = "circuit_open".to_owned();
            snapshot.audit.push("provider.circuit:opened".to_owned());
            snapshot
                .audit
                .push("provider.health:circuit_open".to_owned());
        }
    }
}

/// 执行 `admission_error` 对应的处理逻辑。
fn admission_error(
    request: &ProviderExecutionRequest,
    message: &'static str,
    provider_code: &'static str,
    retry_after_ms: Option<u64>,
) -> EvaError {
    let mut error = EvaError::unavailable(message)
        .with_provider_code(provider_code)
        .with_context("adapter_id", request.adapter_id.as_str())
        .with_context("request_id", request.request_id.as_str())
        .with_context("capability", request.capability.as_str());
    if let Some(retry_after_ms) = retry_after_ms {
        error = error.with_context("retry_after_ms", retry_after_ms.to_string());
    }
    error
}

fn sanitize_drain_value(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn reservation_id_from_audit(snapshot: &ProviderProcessSnapshot) -> Option<String> {
    snapshot.audit.iter().rev().find_map(|entry| {
        entry
            .strip_prefix(PROVIDER_ADMISSION_RESERVATION_AUDIT_PREFIX)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn pending_release_id(snapshot: &ProviderProcessSnapshot) -> Option<String> {
    pending_release_ids(snapshot).pop()
}

fn pending_release_ids(snapshot: &ProviderProcessSnapshot) -> Vec<String> {
    let mut pending = Vec::new();
    for entry in &snapshot.audit {
        let Some(reservation_id) = entry
            .strip_prefix(PROVIDER_ADMISSION_RELEASE_PENDING_AUDIT_PREFIX)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let resolved =
            format!("{PROVIDER_ADMISSION_RELEASE_RESOLVED_AUDIT_PREFIX}{reservation_id}");
        if !snapshot.audit.iter().any(|item| item == &resolved)
            && !pending.iter().any(|item| item == reservation_id)
        {
            pending.push(reservation_id.to_owned());
        }
    }
    pending
}

fn ensure_slot_matches_snapshot(
    slot: &ProviderExecutionSlot,
    snapshot: &ProviderProcessSnapshot,
) -> Result<(), EvaError> {
    if snapshot.provider_process_id == slot.provider_process_id
        && snapshot.request_id == slot.request_id
        && snapshot.adapter_id == slot.adapter_id
    {
        return Ok(());
    }
    Err(
        EvaError::conflict("provider execution slot identity does not match durable snapshot")
            .with_context("session_id", &slot.session_id),
    )
}

fn ensure_request_matches_snapshot(
    request: &ProviderExecutionRequest,
    snapshot: &ProviderProcessSnapshot,
) -> Result<(), EvaError> {
    let matches = snapshot.request_id == request.request_id
        && snapshot.adapter_id == request.adapter_id
        && snapshot.capability == request.capability
        && snapshot.transport == request.transport.as_str()
        && snapshot.manifest_digest == request.manifest_digest
        && snapshot.start_command == request.start_command
        && snapshot.restart_policy == request.provider.restart.mode.as_str()
        && snapshot.restart_max_attempts == request.provider.restart.max_attempts
        && snapshot.restart_backoff_ms == request.provider.restart.backoff_ms;
    if matches {
        return Ok(());
    }
    Err(
        EvaError::conflict("provider restart request does not match its durable session identity")
            .with_context("session_id", &snapshot.session_id)
            .with_context("adapter_id", request.adapter_id.as_str())
            .with_context("request_id", request.request_id.as_str()),
    )
}

/// 执行 `limit_audit_entries` 对应的处理逻辑。
fn limit_audit_entries(request: &ProviderExecutionRequest, half_open_probe: bool) -> Vec<String> {
    let mut audit = vec![
        format!(
            "provider.restart.mode:{}",
            request.provider.restart.mode.as_str()
        ),
        format!(
            "provider.restart.max_attempts:{}",
            request.provider.restart.max_attempts
        ),
        format!(
            "provider.restart.backoff_ms:{}",
            request.provider.restart.backoff_ms
        ),
        format!("provider.run_as.kind:{}", request.provider.run_as.kind()),
        format!(
            "provider.vault_secret_refs:{}",
            request.provider.vault_secrets.len()
        ),
    ];
    if let Some(max_concurrency) = request.max_concurrency {
        audit.push(format!("provider.concurrency.max:{max_concurrency}"));
    }
    if let Some(rate_limit) = request.rate_limit {
        audit.push(format!(
            "provider.rate_limit:{}:{}",
            rate_limit.max_requests, rate_limit.window_ms
        ));
    }
    if let Some(circuit_breaker) = request.circuit_breaker {
        audit.push(format!(
            "provider.circuit.failure_threshold:{}",
            circuit_breaker.failure_threshold
        ));
        audit.push(format!(
            "provider.circuit.recovery_window_ms:{}",
            circuit_breaker.recovery_window_ms
        ));
    }
    if half_open_probe {
        audit.push("provider.circuit:half_open_probe".to_owned());
    }
    audit
}

/// 执行 `session_id` 对应的处理逻辑。
fn session_id(request_id: &RequestId, adapter_id: &AdapterId) -> String {
    format!(
        "session-{}-{}",
        safe_segment(adapter_id.as_str()),
        safe_segment(request_id.as_str())
    )
}

/// 执行 `provider_process_id` 对应的处理逻辑。
fn provider_process_id(request_id: &RequestId, adapter_id: &AdapterId) -> String {
    format!(
        "provider-{}-{}",
        safe_segment(adapter_id.as_str()),
        safe_segment(request_id.as_str())
    )
}

/// 执行 `start_command` 对应的受控流程。
fn start_command(handle: &AdapterHandle) -> String {
    match handle.transport {
        AdapterTransport::Stdio => command_with_args(handle.command.as_deref(), &handle.args),
        AdapterTransport::Http => handle
            .endpoint
            .as_ref()
            .map(|endpoint| {
                format!(
                    "{} {}",
                    handle.method.as_deref().unwrap_or("POST"),
                    endpoint
                )
            })
            .unwrap_or_else(|| "http:<missing-endpoint>".to_owned()),
        AdapterTransport::Mcp => command_with_args(handle.mcp_command.as_deref(), &handle.mcp_args),
        AdapterTransport::Skill => {
            if let Some(command) = handle.skill_runner_command.as_deref() {
                command_with_args(Some(command), &handle.skill_runner_args)
            } else if let Some(command) = handle.command.as_deref() {
                command_with_args(Some(command), &handle.args)
            } else {
                format!(
                    "skill:{}",
                    handle.skill_name().unwrap_or("<missing-skill-id>")
                )
            }
        }
        AdapterTransport::Builtin => "builtin".to_owned(),
        AdapterTransport::Hardware => "hardware-driver".to_owned(),
        AdapterTransport::LuaCapability => "lua-capability".to_owned(),
        AdapterTransport::Eventbus => "eventbus".to_owned(),
    }
}

/// 执行 `command_with_args` 对应的处理逻辑。
fn command_with_args(command: Option<&str>, args: &[String]) -> String {
    let command = command.unwrap_or("<missing-command>");
    if args.is_empty() {
        command.to_owned()
    } else {
        format!("{command} {}", args.join(" "))
    }
}

/// 执行 `manifest_digest` 对应的处理逻辑。
fn manifest_digest(handle: &AdapterHandle) -> String {
    let mut material = Vec::new();
    push_digest_field(&mut material, "format", "eva.adapter.manifest.v4");
    push_digest_field(&mut material, "id", handle.id.as_str());
    push_digest_field(&mut material, "version", &handle.version);
    push_digest_field(&mut material, "transport", handle.transport.as_str());
    push_digest_field(&mut material, "source_path", &handle.source_path);
    push_digest_field(
        &mut material,
        "command",
        handle.command.as_deref().unwrap_or(""),
    );
    push_digest_collection(&mut material, "arg", handle.args.iter().map(String::as_str));
    push_digest_field(
        &mut material,
        "endpoint",
        handle.endpoint.as_deref().unwrap_or(""),
    );
    let mcp_transport = handle
        .mcp_server_transport
        .as_deref()
        .and_then(|transport| eva_mcp::McpServerTransport::parse(transport).ok())
        .map(eva_mcp::McpServerTransport::canonical_str)
        .unwrap_or("");
    push_digest_field(&mut material, "mcp_server_transport", mcp_transport);
    push_digest_field(
        &mut material,
        "mcp_http_config_invalid",
        if handle.mcp_http_config_invalid {
            "true"
        } else {
            "false"
        },
    );
    if let Some(config) = &handle.mcp_http_config {
        push_digest_field(
            &mut material,
            "mcp_endpoint_digest",
            &format!("fnv64:{:016x}", fnv1a64(config.endpoint.as_bytes())),
        );
        push_digest_field(
            &mut material,
            "mcp_endpoint_origin",
            &config.endpoint_origin().unwrap_or_default(),
        );
        push_digest_collection(
            &mut material,
            "mcp_allowed_origin",
            config.allowed_origins.iter().map(String::as_str),
        );
        let trust_root_digests = config
            .trust_roots
            .iter()
            .map(|root| format!("fnv64:{:016x}", fnv1a64(root.as_bytes())))
            .collect::<Vec<_>>();
        push_digest_collection(
            &mut material,
            "mcp_trust_root_digest",
            trust_root_digests.iter().map(String::as_str),
        );
        match config.redirect_policy {
            eva_mcp::McpRedirectPolicy::Deny => {
                push_digest_field(&mut material, "mcp_redirect_mode", "deny");
                push_digest_field(&mut material, "mcp_redirect_max_hops", "0");
            }
            eva_mcp::McpRedirectPolicy::SameOrigin { max_hops } => {
                push_digest_field(&mut material, "mcp_redirect_mode", "same_origin");
                push_digest_field(
                    &mut material,
                    "mcp_redirect_max_hops",
                    &max_hops.to_string(),
                );
            }
        }
        push_digest_field(
            &mut material,
            "mcp_client_auth",
            if config.client_auth.is_some() {
                "configured"
            } else {
                "none"
            },
        );
        if let Some(auth) = &config.client_auth {
            push_digest_field(
                &mut material,
                "mcp_client_cert_ref_digest",
                &format!("fnv64:{:016x}", fnv1a64(auth.certificate_ref.as_bytes())),
            );
            push_digest_field(
                &mut material,
                "mcp_client_key_ref_digest",
                &format!("fnv64:{:016x}", fnv1a64(auth.private_key_ref.as_bytes())),
            );
        }
    }
    push_digest_field(
        &mut material,
        "mcp_header_count",
        &handle.headers.len().to_string(),
    );
    for (name, value) in &handle.headers {
        push_digest_field(&mut material, "mcp_header_name", name);
        push_digest_field(
            &mut material,
            "mcp_header_value_digest",
            &format!("fnv64:{:016x}", fnv1a64(value.as_bytes())),
        );
    }
    push_digest_field(
        &mut material,
        "mcp_command",
        handle.mcp_command.as_deref().unwrap_or(""),
    );
    push_digest_collection(
        &mut material,
        "mcp_arg",
        handle.mcp_args.iter().map(String::as_str),
    );
    push_digest_field(
        &mut material,
        "skill_runner_command",
        handle.skill_runner_command.as_deref().unwrap_or(""),
    );
    push_digest_collection(
        &mut material,
        "skill_runner_arg",
        handle.skill_runner_args.iter().map(String::as_str),
    );
    push_digest_field(
        &mut material,
        "skill_name",
        handle.skill_name().unwrap_or(""),
    );

    let mut capabilities = handle
        .capabilities
        .iter()
        .map(|capability| capability.as_str())
        .collect::<Vec<_>>();
    capabilities.sort_unstable();
    push_digest_collection(&mut material, "capability", capabilities);

    let mut credential_env = handle
        .credential_env
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    credential_env.sort_unstable();
    push_digest_collection(&mut material, "credential_env", credential_env);

    push_digest_field(
        &mut material,
        "restart_mode",
        handle.provider.restart.mode.as_str(),
    );
    push_digest_field(
        &mut material,
        "restart_max_attempts",
        &handle.provider.restart.max_attempts.to_string(),
    );
    push_digest_field(
        &mut material,
        "restart_backoff_ms",
        &handle.provider.restart.backoff_ms.to_string(),
    );
    match &handle.provider.run_as {
        ProviderRunAsIdentity::Current => {
            push_digest_field(&mut material, "run_as_kind", "current");
        }
        ProviderRunAsIdentity::Unix { uid, gid } => {
            push_digest_field(&mut material, "run_as_kind", "unix");
            push_digest_field(&mut material, "run_as_uid", &uid.to_string());
            push_digest_field(&mut material, "run_as_gid", &gid.to_string());
        }
        ProviderRunAsIdentity::Windows { account } => {
            push_digest_field(&mut material, "run_as_kind", "windows");
            push_digest_field(&mut material, "run_as_account", account);
        }
    }

    let mut vault_secrets = handle.provider.vault_secrets.iter().collect::<Vec<_>>();
    vault_secrets.sort_unstable();
    push_digest_field(
        &mut material,
        "vault_secret_count",
        &vault_secrets.len().to_string(),
    );
    for secret in vault_secrets {
        push_digest_field(&mut material, "vault_env", &secret.env);
        push_digest_field(&mut material, "vault_ref", &secret.secret_ref);
    }

    format!("fnv64:{:016x}", fnv1a64(&material))
}

/// Appends one labeled, length-prefixed field to canonical digest material.
fn push_digest_field(material: &mut Vec<u8>, label: &str, value: &str) {
    push_digest_bytes(material, label.as_bytes());
    push_digest_bytes(material, value.as_bytes());
}

/// Appends an ordered collection with an explicit count and repeated field label.
fn push_digest_collection<'a>(
    material: &mut Vec<u8>,
    label: &str,
    values: impl IntoIterator<Item = &'a str>,
) {
    let values = values.into_iter().collect::<Vec<_>>();
    push_digest_field(
        material,
        &format!("{label}_count"),
        &values.len().to_string(),
    );
    for value in values {
        push_digest_field(material, label, value);
    }
}

/// Uses a platform-independent u64 byte length before every digest component.
fn push_digest_bytes(material: &mut Vec<u8>, value: &[u8]) {
    material.extend_from_slice(&(value.len() as u64).to_be_bytes());
    material.extend_from_slice(value);
}

/// 执行 `credential_token_digest` 对应的处理逻辑。
fn credential_token_digest(
    session_id: &str,
    adapter_id: &AdapterId,
    request_id: &RequestId,
    capability: &CapabilityName,
) -> String {
    let material = format!(
        "{session_id}|{}|{}|{}",
        adapter_id.as_str(),
        request_id.as_str(),
        capability.as_str()
    );
    format!("fnv64:{:016x}", fnv1a64(material.as_bytes()))
}

/// 执行 `fnv1a64` 对应的处理逻辑。
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// 执行 `safe_segment` 对应的处理逻辑。
fn safe_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// 执行 `now_ms` 对应的处理逻辑。
fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_vault::{CredentialSession, CredentialVault, SecretValue};
    use crate::manifest::AdapterHandle;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// 执行 `handle` 对应的处理逻辑。
    fn handle() -> AdapterHandle {
        AdapterHandle {
            id: AdapterId::parse("stdio-test").unwrap(),
            name: "Stdio Test".to_owned(),
            version: "1.0.0".to_owned(),
            enabled: true,
            transport: AdapterTransport::Stdio,
            capabilities: vec![CapabilityName::parse("repo.analyze").unwrap()],
            source_path: "test".to_owned(),
            project_root: None,
            command: Some("stdio-runner".to_owned()),
            args: vec!["--once".to_owned()],
            endpoint: None,
            method: None,
            credential_env: Vec::new(),
            provider: ProviderConfig::default(),
            timeout_ms: Some(5_000),
            max_concurrency: None,
            output_limit_bytes: Some(4096),
            max_prompt_bytes: Some(4096),
            rate_limit: None,
            circuit_breaker: None,
            headers: BTreeMap::new(),
            mcp_server_transport: None,
            mcp_command: None,
            mcp_args: Vec::new(),
            mcp_tools: Vec::new(),
            mcp_http_config: None,
            mcp_http_config_invalid: false,
            skill_id: None,
            skill_kind: None,
            skill_runtime_gate: None,
            skill_path: None,
            skill_entry_type: None,
            skill_runner_command: None,
            skill_runner_args: Vec::new(),
            skill_artifact_root: None,
            skill_input_schema: None,
            hardware_logical_name: None,
            hardware_device_class: None,
            hardware_driver_id: None,
            hardware_driver_kind: None,
            bindings: Vec::new(),
        }
    }

    /// 执行 `invocation` 对应的处理逻辑。
    fn invocation(request_id: &str) -> AdapterInvocation {
        AdapterInvocation::new(
            RequestId::parse(request_id).unwrap(),
            CapabilityName::parse("repo.analyze").unwrap(),
        )
    }

    #[derive(Clone)]
    struct ReleaseTestVault {
        attempts: Arc<AtomicUsize>,
        failures_remaining: Arc<AtomicUsize>,
        delay: Duration,
        reentrant_lifecycle: Option<McpHttpLifecycleHandle>,
        reentrant_lock_observed: Arc<AtomicBool>,
    }

    impl fmt::Debug for ReleaseTestVault {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("ReleaseTestVault([REDACTED])")
        }
    }

    impl CredentialVault for ReleaseTestVault {
        fn open_session(
            &self,
            _scope: &ProviderCredentialScope,
        ) -> Result<Box<dyn CredentialSession>, EvaError> {
            Ok(Box::new(ReleaseTestSession {
                attempts: Arc::clone(&self.attempts),
                failures_remaining: Arc::clone(&self.failures_remaining),
                delay: self.delay,
                reentrant_lifecycle: self.reentrant_lifecycle.clone(),
                reentrant_lock_observed: Arc::clone(&self.reentrant_lock_observed),
                released: false,
            }))
        }
    }

    struct ReleaseTestSession {
        attempts: Arc<AtomicUsize>,
        failures_remaining: Arc<AtomicUsize>,
        delay: Duration,
        reentrant_lifecycle: Option<McpHttpLifecycleHandle>,
        reentrant_lock_observed: Arc<AtomicBool>,
        released: bool,
    }

    impl fmt::Debug for ReleaseTestSession {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("ReleaseTestSession")
                .field("released", &self.released)
                .finish()
        }
    }

    impl CredentialSession for ReleaseTestSession {
        fn fetch(&mut self, _secret_ref: &str) -> Result<SecretValue, EvaError> {
            if self.released {
                return Err(EvaError::conflict(
                    "release test credential session is closed",
                ));
            }
            Ok(SecretValue::new("release-test-secret"))
        }

        fn release(&mut self) -> Result<(), EvaError> {
            if self.released {
                return Ok(());
            }
            self.attempts.fetch_add(1, Ordering::SeqCst);
            if !self.delay.is_zero() {
                thread::sleep(self.delay);
            }
            if let Some(lifecycle) = &self.reentrant_lifecycle {
                for _ in 0..1_000 {
                    if lifecycle.try_lock().is_ok() {
                        self.reentrant_lock_observed.store(true, Ordering::SeqCst);
                        break;
                    }
                    thread::yield_now();
                }
            }
            let should_fail = self
                .failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok();
            if should_fail {
                return Err(EvaError::unavailable(
                    "release test vault rejected credential revocation",
                ));
            }
            self.released = true;
            Ok(())
        }
    }

    struct BlockingReleaseVault {
        started: mpsc::SyncSender<()>,
        unblock: Arc<Mutex<Option<mpsc::Receiver<()>>>>,
        finished: mpsc::SyncSender<()>,
    }

    impl fmt::Debug for BlockingReleaseVault {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("BlockingReleaseVault([REDACTED])")
        }
    }

    impl CredentialVault for BlockingReleaseVault {
        fn open_session(
            &self,
            _scope: &ProviderCredentialScope,
        ) -> Result<Box<dyn CredentialSession>, EvaError> {
            let unblock = self
                .unblock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
                .expect("blocking release vault opens one test session");
            Ok(Box::new(BlockingReleaseSession {
                started: self.started.clone(),
                unblock,
                finished: self.finished.clone(),
                released: false,
            }))
        }
    }

    struct BlockingReleaseSession {
        started: mpsc::SyncSender<()>,
        unblock: mpsc::Receiver<()>,
        finished: mpsc::SyncSender<()>,
        released: bool,
    }

    impl fmt::Debug for BlockingReleaseSession {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("BlockingReleaseSession")
                .field("released", &self.released)
                .finish()
        }
    }

    impl CredentialSession for BlockingReleaseSession {
        fn fetch(&mut self, _secret_ref: &str) -> Result<SecretValue, EvaError> {
            Ok(SecretValue::new("blocking-release-test-secret"))
        }

        fn release(&mut self) -> Result<(), EvaError> {
            if self.released {
                return Ok(());
            }
            let _ = self.started.send(());
            self.unblock
                .recv()
                .map_err(|_| EvaError::internal("blocking release test lost its unblock signal"))?;
            self.released = true;
            let _ = self.finished.send(());
            Ok(())
        }
    }

    fn release_test_lease(vault: &ReleaseTestVault) -> CredentialSessionLease {
        let scope = ProviderCredentialScope::new_for_session(
            "release-test-session",
            AdapterId::parse("release-test-provider").unwrap(),
            RequestId::parse("req-release-test").unwrap(),
            CapabilityName::parse("vault.release").unwrap(),
        );
        CredentialSessionLease::open(vault, Some(&scope), &[], &["RELEASE_TEST_TOKEN".to_owned()])
            .unwrap()
    }

    fn blocking_release_test_lease(vault: &BlockingReleaseVault) -> CredentialSessionLease {
        let scope = ProviderCredentialScope::new_for_session(
            "blocking-release-test-session",
            AdapterId::parse("blocking-release-test-provider").unwrap(),
            RequestId::parse("req-blocking-release-test").unwrap(),
            CapabilityName::parse("vault.release").unwrap(),
        );
        CredentialSessionLease::open(
            vault,
            Some(&scope),
            &[],
            &["BLOCKING_RELEASE_TEST_TOKEN".to_owned()],
        )
        .unwrap()
    }

    fn add_completed_credential_owner(
        lifecycle: &McpHttpLifecycleHandle,
        session_id: &str,
        credentials: CredentialSessionLease,
    ) {
        lifecycle
            .try_lock()
            .unwrap()
            .credential_leases
            .insert(session_id.to_owned(), credentials);
    }

    /// 验证 `supervisor_records_acquire_and_release` 场景下的预期行为。
    #[test]
    fn supervisor_records_acquire_and_release() {
        let handle = handle();
        let invocation = invocation("req-supervisor-1");
        let mut supervisor = InMemoryProviderSupervisor::new();

        let slot = supervisor
            .acquire(ProviderExecutionRequest::from_handle(&handle, &invocation))
            .unwrap();
        assert_eq!(supervisor.active_for_adapter(&handle.id).unwrap().len(), 1);

        let snapshot = supervisor
            .complete(&slot, ProviderExecutionOutcome::completed("completed"))
            .unwrap();

        assert!(!snapshot.active);
        assert_eq!(snapshot.health, "completed");
        assert_eq!(supervisor.active_for_adapter(&handle.id).unwrap().len(), 0);
        assert!(snapshot
            .audit
            .iter()
            .any(|entry| entry == "provider.slot:released"));
    }

    #[test]
    fn supervisor_drain_closes_admission_and_is_idempotent() {
        let handle = handle();
        let initial_invocation = invocation("req-supervisor-drain");
        let mut supervisor = InMemoryProviderSupervisor::new();
        let slot = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &initial_invocation,
            ))
            .unwrap();

        let options = ProviderDrainOptions::new(Duration::from_millis(50))
            .unwrap()
            .with_poll_interval(Duration::from_millis(1))
            .unwrap();
        let report = supervisor.drain(options).unwrap();
        assert_eq!(report.phase, "drained");
        assert_eq!(report.active_provider_count, 0);
        assert!(!report.already_drained);
        assert_eq!(report.mcp_http_sessions_before, 0);
        assert_eq!(report.mcp_http_readers_before, 0);
        assert_eq!(report.mcp_http_sessions_after, 0);
        assert_eq!(report.mcp_http_readers_after, 0);
        assert_eq!(report.mcp_http_cleanup_pending_after, 0);
        assert!(supervisor.is_draining());
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "provider.lifecycle:admission_closed"));
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "mcp.http_registry:admission_closed"));

        let rejected = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-drain-after"),
            ))
            .unwrap_err();
        assert_eq!(
            rejected.provider_code().map(|code| code.as_str()),
            Some("provider_draining")
        );

        let repeated = supervisor.drain(options).unwrap();
        assert!(repeated.already_drained);
        assert_eq!(repeated.phase, "drained");
        assert!(supervisor
            .complete(&slot, ProviderExecutionOutcome::completed("completed"))
            .is_err());

        let registration_error = supervisor
            .register_process_identity(
                &slot,
                &ProcessIdentity {
                    pid: 1,
                    process_start_token: "drain-test".to_owned(),
                    process_group_id: Some(1),
                    job_id: None,
                },
            )
            .unwrap_err();
        assert_eq!(
            registration_error.provider_code().map(|code| code.as_str()),
            Some("provider_draining")
        );
    }

    #[test]
    fn supervisor_drain_times_out_without_detaching_slow_credential_release() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut supervisor = InMemoryProviderSupervisor::new();
        let lifecycle = supervisor.mcp_http_lifecycle();
        let vault = ReleaseTestVault {
            attempts: Arc::clone(&attempts),
            failures_remaining: Arc::new(AtomicUsize::new(0)),
            delay: Duration::from_millis(300),
            reentrant_lifecycle: None,
            reentrant_lock_observed: Arc::new(AtomicBool::new(false)),
        };
        add_completed_credential_owner(
            &lifecycle,
            "slow-release-session",
            release_test_lease(&vault),
        );

        let started = Instant::now();
        let error = supervisor
            .drain(ProviderDrainOptions::new(Duration::from_millis(20)).unwrap())
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "credential release exceeded the bounded drain budget: {:?}",
            started.elapsed()
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        {
            let state = lifecycle.try_lock().unwrap();
            assert_eq!(state.credential_leases.len(), 0);
            assert_eq!(state.credential_releases.len(), 1);
            assert!(state.credential_releases["slow-release-session"]
                .worker
                .handle
                .lock()
                .unwrap()
                .is_some());
        }

        thread::sleep(Duration::from_millis(320));
        let report = supervisor
            .drain(ProviderDrainOptions::new(Duration::from_secs(1)).unwrap())
            .unwrap();
        assert_eq!(report.phase, "drained");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        let state = lifecycle.try_lock().unwrap();
        assert!(state.credential_leases.is_empty());
        assert!(state.credential_releases.is_empty());
    }

    #[test]
    fn dropping_lifecycle_defers_a_running_credential_worker_to_the_reaper() {
        let _guard = credential_finalizer_test_guard();
        credential_join_reaper().unwrap();
        let (started_sender, started) = mpsc::sync_channel(1);
        let (unblock, unblock_receiver) = mpsc::sync_channel(1);
        let (finished_sender, finished) = mpsc::sync_channel(1);
        let vault = BlockingReleaseVault {
            started: started_sender,
            unblock: Arc::new(Mutex::new(Some(unblock_receiver))),
            finished: finished_sender,
        };
        let lifecycle = McpHttpLifecycleHandle::new();
        add_completed_credential_owner(
            &lifecycle,
            "blocking-release-session",
            blocking_release_test_lease(&vault),
        );

        let error = lifecycle
            .drain_all_until(Instant::now() + Duration::from_millis(20))
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Timeout);
        started.recv_timeout(Duration::from_secs(1)).unwrap();

        let drop_started = Instant::now();
        drop(lifecycle);
        assert!(
            drop_started.elapsed() < Duration::from_millis(200),
            "lifecycle drop waited for a running credential worker: {:?}",
            drop_started.elapsed()
        );

        let residual = drain_credential_finalizer_while_test_guarded(
            Instant::now() + Duration::from_millis(20),
        )
        .unwrap_err();
        assert_eq!(
            residual.provider_code().map(|code| code.as_str()),
            Some("credential_finalizer_drain_timeout")
        );

        unblock.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(1)).unwrap();
        drain_credential_finalizer_while_test_guarded(Instant::now() + Duration::from_secs(1))
            .unwrap();
    }

    #[test]
    fn dropping_lifecycle_defers_pending_credential_owner_to_the_finalizer() {
        let _guard = credential_finalizer_test_guard();
        let (started_sender, started) = mpsc::sync_channel(1);
        let (unblock, unblock_receiver) = mpsc::sync_channel(1);
        let (finished_sender, finished) = mpsc::sync_channel(1);
        let vault = BlockingReleaseVault {
            started: started_sender,
            unblock: Arc::new(Mutex::new(Some(unblock_receiver))),
            finished: finished_sender,
        };
        let lifecycle = McpHttpLifecycleHandle::new();
        let prepared = lifecycle
            .try_lock()
            .unwrap()
            .prepare_detached_credential_release(blocking_release_test_lease(&vault));
        drop(prepared);

        let drop_started = Instant::now();
        drop(lifecycle);
        assert!(
            drop_started.elapsed() < Duration::from_millis(200),
            "lifecycle drop ran a pending credential release synchronously: {:?}",
            drop_started.elapsed()
        );

        started.recv_timeout(Duration::from_secs(1)).unwrap();

        let residual = drain_credential_finalizer_while_test_guarded(
            Instant::now() + Duration::from_millis(20),
        )
        .unwrap_err();
        assert_eq!(
            residual.provider_code().map(|code| code.as_str()),
            Some("credential_finalizer_drain_timeout")
        );
        unblock.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(1)).unwrap();
        drain_credential_finalizer_while_test_guarded(Instant::now() + Duration::from_secs(1))
            .unwrap();
    }

    #[test]
    fn dropping_lifecycle_retains_failed_owner_until_authorized_finalizer_retry() {
        let _guard = credential_finalizer_test_guard();
        let attempts = Arc::new(AtomicUsize::new(0));
        let vault = ReleaseTestVault {
            attempts: Arc::clone(&attempts),
            failures_remaining: Arc::new(AtomicUsize::new(2)),
            delay: Duration::ZERO,
            reentrant_lifecycle: None,
            reentrant_lock_observed: Arc::new(AtomicBool::new(false)),
        };
        let lifecycle = McpHttpLifecycleHandle::new();

        let error = lifecycle
            .release_detached_credentials_unchecked(release_test_lease(&vault))
            .unwrap_err();
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("credential_vault_release_error")
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        {
            let state = lifecycle.try_lock().unwrap();
            assert_eq!(state.credential_leases.len(), 1);
            assert!(state.credential_releases.is_empty());
        }

        drop(lifecycle);
        let deadline = Instant::now() + Duration::from_secs(1);
        while attempts.load(Ordering::SeqCst) < 2 {
            assert!(
                Instant::now() < deadline,
                "finalizer did not retry the failed credential owner"
            );
            thread::yield_now();
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        thread::sleep(Duration::from_millis(50));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "a failed finalizer owner must wait for an authorized drain retry"
        );

        drain_credential_finalizer_while_test_guarded(Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn expired_finalizer_authorization_does_not_start_a_late_retry() {
        let _guard = credential_finalizer_test_guard();
        let attempts = Arc::new(AtomicUsize::new(0));
        let vault = ReleaseTestVault {
            attempts: Arc::clone(&attempts),
            failures_remaining: Arc::new(AtomicUsize::new(1)),
            delay: Duration::from_millis(50),
            reentrant_lifecycle: None,
            reentrant_lock_observed: Arc::new(AtomicBool::new(false)),
        };
        let lifecycle = McpHttpLifecycleHandle::new();
        add_completed_credential_owner(
            &lifecycle,
            "expired-finalizer-authorization",
            release_test_lease(&vault),
        );
        drop(lifecycle);

        let deadline = Instant::now() + Duration::from_secs(1);
        while attempts.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }

        let error = drain_credential_finalizer_while_test_guarded(
            Instant::now() + Duration::from_millis(20),
        )
        .unwrap_err();
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("credential_finalizer_drain_timeout")
        );
        thread::sleep(Duration::from_millis(80));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "an expired drain authorization started a late credential retry"
        );

        drain_credential_finalizer_while_test_guarded(Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn credential_finalizer_does_not_block_fast_release_behind_slow_release() {
        let _guard = credential_finalizer_test_guard();
        let (slow_started_sender, slow_started) = mpsc::sync_channel(1);
        let (slow_unblock, slow_unblock_receiver) = mpsc::sync_channel(1);
        let (slow_finished_sender, slow_finished) = mpsc::sync_channel(1);
        let slow_vault = BlockingReleaseVault {
            started: slow_started_sender,
            unblock: Arc::new(Mutex::new(Some(slow_unblock_receiver))),
            finished: slow_finished_sender,
        };
        let slow_lifecycle = McpHttpLifecycleHandle::new();
        add_completed_credential_owner(
            &slow_lifecycle,
            "slow-finalizer-release",
            blocking_release_test_lease(&slow_vault),
        );
        drop(slow_lifecycle);
        slow_started.recv_timeout(Duration::from_secs(1)).unwrap();

        let fast_attempts = Arc::new(AtomicUsize::new(0));
        let fast_vault = ReleaseTestVault {
            attempts: Arc::clone(&fast_attempts),
            failures_remaining: Arc::new(AtomicUsize::new(0)),
            delay: Duration::ZERO,
            reentrant_lifecycle: None,
            reentrant_lock_observed: Arc::new(AtomicBool::new(false)),
        };
        let fast_lifecycle = McpHttpLifecycleHandle::new();
        add_completed_credential_owner(
            &fast_lifecycle,
            "fast-finalizer-release",
            release_test_lease(&fast_vault),
        );
        drop(fast_lifecycle);

        let deadline = Instant::now() + Duration::from_secs(1);
        while fast_attempts.load(Ordering::SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "fast credential release was blocked behind a slow finalizer task"
            );
            thread::yield_now();
        }
        assert!(matches!(slow_finished.try_recv(), Err(TryRecvError::Empty)));

        slow_unblock.send(()).unwrap();
        slow_finished.recv_timeout(Duration::from_secs(1)).unwrap();
        drain_credential_finalizer_while_test_guarded(Instant::now() + Duration::from_secs(1))
            .unwrap();
    }

    #[test]
    fn credential_join_reaper_does_not_block_fast_jobs_behind_a_slow_worker() {
        credential_join_reaper().unwrap();
        let (unblock, blocked) = mpsc::sync_channel(1);
        let slow_worker = thread::spawn(move || blocked.recv().unwrap());
        let (slow_ack_sender, slow_ack) = mpsc::sync_channel(1);
        defer_credential_join(slow_worker, Some(slow_ack_sender), None);

        let fast_worker = thread::spawn(|| {});
        let (fast_ack_sender, fast_ack) = mpsc::sync_channel(1);
        defer_credential_join(fast_worker, Some(fast_ack_sender), None);

        assert!(fast_ack.recv_timeout(Duration::from_secs(1)).unwrap());
        assert!(matches!(slow_ack.try_recv(), Err(TryRecvError::Empty)));
        unblock.send(()).unwrap();
        assert!(slow_ack.recv_timeout(Duration::from_secs(1)).unwrap());
    }

    #[test]
    fn credential_join_reaper_continues_after_a_panicking_worker() {
        credential_join_reaper().unwrap();
        let panicking_worker = thread::spawn(|| panic!("credential worker test panic"));
        let (panic_ack_sender, panic_ack) = mpsc::sync_channel(1);
        defer_credential_join(panicking_worker, Some(panic_ack_sender), None);
        assert!(!panic_ack.recv_timeout(Duration::from_secs(1)).unwrap());

        let healthy_worker = thread::spawn(|| {});
        let (healthy_ack_sender, healthy_ack) = mpsc::sync_channel(1);
        defer_credential_join(healthy_worker, Some(healthy_ack_sender), None);
        assert!(healthy_ack.recv_timeout(Duration::from_secs(1)).unwrap());
    }

    #[test]
    fn completed_credential_release_joins_its_worker_synchronously() {
        let lifecycle = McpHttpLifecycleHandle::new();
        let vault = ReleaseTestVault {
            attempts: Arc::new(AtomicUsize::new(0)),
            failures_remaining: Arc::new(AtomicUsize::new(0)),
            delay: Duration::ZERO,
            reentrant_lifecycle: None,
            reentrant_lock_observed: Arc::new(AtomicBool::new(false)),
        };
        let prepared = lifecycle
            .try_lock()
            .unwrap()
            .prepare_detached_credential_release(release_test_lease(&vault));
        let worker = Arc::clone(&prepared.waiter.worker);
        let waiter = lifecycle.launch_credential_release(prepared, None).unwrap();

        lifecycle
            .complete_credential_release(&waiter, None)
            .unwrap();
        assert!(worker
            .handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_none());
    }

    #[test]
    fn expired_credential_worker_deadline_preserves_owner_without_calling_release() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let lifecycle = McpHttpLifecycleHandle::new();
        let vault = ReleaseTestVault {
            attempts: Arc::clone(&attempts),
            failures_remaining: Arc::new(AtomicUsize::new(0)),
            delay: Duration::ZERO,
            reentrant_lifecycle: None,
            reentrant_lock_observed: Arc::new(AtomicBool::new(false)),
        };
        let prepared = lifecycle
            .try_lock()
            .unwrap()
            .prepare_detached_credential_release(release_test_lease(&vault));
        let waiter = lifecycle
            .launch_credential_release(prepared, Some(Instant::now()))
            .unwrap();

        let error = lifecycle
            .complete_credential_release(&waiter, None)
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("credential_release_timeout")
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 0);
        let state = lifecycle.try_lock().unwrap();
        assert_eq!(state.credential_leases.len(), 1);
        assert!(state.credential_releases.is_empty());
    }

    #[test]
    fn supervisor_drain_retries_failed_credential_release_without_false_success() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut supervisor = InMemoryProviderSupervisor::new();
        let lifecycle = supervisor.mcp_http_lifecycle();
        let vault = ReleaseTestVault {
            attempts: Arc::clone(&attempts),
            failures_remaining: Arc::new(AtomicUsize::new(1)),
            delay: Duration::ZERO,
            reentrant_lifecycle: None,
            reentrant_lock_observed: Arc::new(AtomicBool::new(false)),
        };
        add_completed_credential_owner(
            &lifecycle,
            "retry-release-session",
            release_test_lease(&vault),
        );

        let first = supervisor
            .drain(ProviderDrainOptions::new(Duration::from_secs(1)).unwrap())
            .unwrap_err();
        assert_eq!(
            first.provider_code().map(|code| code.as_str()),
            Some("credential_vault_release_error")
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        {
            let state = lifecycle.try_lock().unwrap();
            assert_eq!(state.credential_leases.len(), 1);
            assert!(state.credential_releases.is_empty());
        }

        let report = supervisor
            .drain(ProviderDrainOptions::new(Duration::from_secs(1)).unwrap())
            .unwrap();
        assert_eq!(report.phase, "drained");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        let state = lifecycle.try_lock().unwrap();
        assert!(state.credential_leases.is_empty());
        assert!(state.credential_releases.is_empty());
    }

    #[test]
    fn supervisor_drain_waits_for_http_operations_and_keeps_admission_closed() {
        let mut supervisor = InMemoryProviderSupervisor::new();
        let lifecycle = supervisor.mcp_http_lifecycle();
        let operation = lifecycle.begin_operation().unwrap();

        let started = Instant::now();
        let first = supervisor
            .drain(ProviderDrainOptions::new(Duration::from_millis(20)).unwrap())
            .unwrap_err();
        assert_eq!(first.kind(), ErrorKind::Timeout);
        assert_eq!(
            first.provider_code().map(|code| code.as_str()),
            Some("mcp_http_operation_drain_timeout")
        );
        assert!(started.elapsed() < Duration::from_millis(200));
        assert_eq!(
            lifecycle
                .begin_operation()
                .unwrap_err()
                .provider_code()
                .map(|code| code.as_str()),
            Some("mcp_http_registry_admission_closed")
        );

        drop(operation);
        let report = supervisor
            .drain(ProviderDrainOptions::new(Duration::from_secs(1)).unwrap())
            .unwrap();
        assert_eq!(report.phase, "drained");
        assert_eq!(
            lifecycle
                .begin_operation()
                .unwrap_err()
                .provider_code()
                .map(|code| code.as_str()),
            Some("mcp_http_registry_admission_closed")
        );
    }

    #[test]
    fn http_drain_deadline_includes_operation_gate_acquisition() {
        let lifecycle = McpHttpLifecycleHandle::new();
        let gate = lifecycle.0.operation_gate.lock().unwrap();
        let draining_lifecycle = lifecycle.clone();
        let deadline = Instant::now() + Duration::from_millis(20);
        let started = Instant::now();
        let drain = thread::spawn(move || draining_lifecycle.drain_all_until(deadline));

        let error = drain.join().unwrap().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("mcp_http_operation_drain_timeout")
        );
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "operation_gate_busy" && value == "true"));
        assert!(started.elapsed() < Duration::from_millis(200));
        drop(gate);
        assert_eq!(
            lifecycle
                .begin_operation()
                .unwrap_err()
                .provider_code()
                .map(|code| code.as_str()),
            Some("mcp_http_registry_admission_closed")
        );
    }

    #[test]
    fn drain_rescans_credentials_created_by_an_admitted_http_operation() {
        let lifecycle = McpHttpLifecycleHandle::new();
        let operation = lifecycle.begin_operation().unwrap();
        let draining_lifecycle = lifecycle.clone();
        let drain = thread::spawn(move || {
            draining_lifecycle.drain_all_until(Instant::now() + Duration::from_secs(2))
        });

        let admission_deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let closed = lifecycle.0.admission_closed.load(Ordering::Acquire);
            if closed {
                break;
            }
            assert!(Instant::now() < admission_deadline);
            thread::yield_now();
        }

        let attempts = Arc::new(AtomicUsize::new(0));
        let vault = ReleaseTestVault {
            attempts: Arc::clone(&attempts),
            failures_remaining: Arc::new(AtomicUsize::new(1)),
            delay: Duration::ZERO,
            reentrant_lifecycle: None,
            reentrant_lock_observed: Arc::new(AtomicBool::new(false)),
        };
        let first_release = lifecycle
            .release_detached_credentials(&operation, release_test_lease(&vault))
            .unwrap_err();
        assert_eq!(
            first_release.provider_code().map(|code| code.as_str()),
            Some("credential_vault_release_error")
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        drop(operation);
        let report = drain.join().unwrap().unwrap();
        assert!(report.complete);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        let state = lifecycle.try_lock().unwrap();
        assert!(state.credential_leases.is_empty());
        assert!(state.credential_releases.is_empty());
    }

    #[test]
    fn mismatched_permit_keeps_credentials_with_its_source_lifecycle() {
        let source = McpHttpLifecycleHandle::new();
        let target = McpHttpLifecycleHandle::new();
        target
            .drain_all_until(Instant::now() + Duration::from_secs(1))
            .unwrap();
        let operation = source.begin_operation().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let vault = ReleaseTestVault {
            attempts: Arc::clone(&attempts),
            failures_remaining: Arc::new(AtomicUsize::new(1)),
            delay: Duration::ZERO,
            reentrant_lifecycle: None,
            reentrant_lock_observed: Arc::new(AtomicBool::new(false)),
        };

        let session = McpStreamableHttpSession::new(
            eva_mcp::McpStreamableHttpConfig::legacy_http("http://127.0.0.1:9/mcp").unwrap(),
            BTreeMap::new(),
            Duration::from_secs(1),
            1024,
        )
        .unwrap();
        let mismatch = target
            .register_starting_session(
                &operation,
                AdapterId::parse("mismatched-permit-provider").unwrap(),
                session,
                release_test_lease(&vault),
            )
            .unwrap_err();
        assert_eq!(
            mismatch.provider_code().map(|code| code.as_str()),
            Some("mcp_http_operation_permit_mismatch")
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        {
            let state = target.try_lock().unwrap();
            assert!(state.credential_leases.is_empty());
            assert!(state.credential_releases.is_empty());
        }

        drop(operation);
        source
            .drain_all_until(Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        target
            .drain_all_until(Instant::now() + Duration::from_secs(1))
            .unwrap();
        let state = target.try_lock().unwrap();
        assert!(state.credential_leases.is_empty());
        assert!(state.credential_releases.is_empty());
    }

    #[test]
    fn busy_registration_tracks_credentials_before_returning() {
        let lifecycle = McpHttpLifecycleHandle::new();
        let operation = lifecycle.begin_operation().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let vault = ReleaseTestVault {
            attempts: Arc::clone(&attempts),
            failures_remaining: Arc::new(AtomicUsize::new(1)),
            delay: Duration::ZERO,
            reentrant_lifecycle: None,
            reentrant_lock_observed: Arc::new(AtomicBool::new(false)),
        };
        let credentials = release_test_lease(&vault);
        let session = McpStreamableHttpSession::new(
            eva_mcp::McpStreamableHttpConfig::legacy_http("http://127.0.0.1:9/mcp").unwrap(),
            BTreeMap::new(),
            Duration::from_secs(1),
            1024,
        )
        .unwrap();
        let lifecycle_lock = lifecycle.try_lock().unwrap();
        let registering_lifecycle = lifecycle.clone();
        let (started_sender, started) = std::sync::mpsc::sync_channel(1);
        let registration = thread::spawn(move || {
            started_sender.send(()).unwrap();
            registering_lifecycle.register_starting_session(
                &operation,
                AdapterId::parse("busy-registration-provider").unwrap(),
                session,
                credentials,
            )
        });
        started.recv().unwrap();
        thread::sleep(Duration::from_millis(10));
        assert!(!registration.is_finished());

        drop(lifecycle_lock);
        let error = registration.join().unwrap().unwrap_err();
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("mcp_http_registry_busy")
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        lifecycle
            .drain_all_until(Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn credential_release_callback_runs_without_the_lifecycle_lock() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let reentrant_lock_observed = Arc::new(AtomicBool::new(false));
        let mut supervisor = InMemoryProviderSupervisor::new();
        let lifecycle = supervisor.mcp_http_lifecycle();
        let vault = ReleaseTestVault {
            attempts: Arc::clone(&attempts),
            failures_remaining: Arc::new(AtomicUsize::new(0)),
            delay: Duration::ZERO,
            reentrant_lifecycle: Some(lifecycle.clone()),
            reentrant_lock_observed: Arc::clone(&reentrant_lock_observed),
        };
        add_completed_credential_owner(
            &lifecycle,
            "reentrant-release-session",
            release_test_lease(&vault),
        );

        supervisor
            .drain(ProviderDrainOptions::new(Duration::from_secs(1)).unwrap())
            .unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(reentrant_lock_observed.load(Ordering::SeqCst));
    }

    #[test]
    fn finished_credential_release_cannot_succeed_after_its_deadline() {
        let vault = ReleaseTestVault {
            attempts: Arc::new(AtomicUsize::new(0)),
            failures_remaining: Arc::new(AtomicUsize::new(0)),
            delay: Duration::ZERO,
            reentrant_lifecycle: None,
            reentrant_lock_observed: Arc::new(AtomicBool::new(false)),
        };
        let mut credentials = release_test_lease(&vault);
        credentials.release().unwrap();
        let waiter = CredentialReleaseWaiter {
            session_id: "finished-after-deadline".to_owned(),
            completion: Arc::new(CredentialReleaseCompletion {
                state: Mutex::new(CredentialReleaseCompletionState::Finished {
                    credentials,
                    result: Ok(Vec::new()),
                }),
                ready: Condvar::new(),
            }),
            worker: Arc::new(CredentialReleaseWorker {
                handle: Mutex::new(None),
                start_gate: Arc::new(CredentialReleaseStartGate {
                    attached: Mutex::new(false),
                    ready: Condvar::new(),
                }),
                finalizer_scope: 0,
            }),
        };

        let error = waiter.wait_until(Some(Instant::now())).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("credential_release_timeout")
        );
    }

    #[test]
    fn supervisor_drain_keeps_legacy_identityless_snapshot_fail_closed() {
        let handle = handle();
        let mut supervisor = InMemoryProviderSupervisor::new();
        let slot = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-legacy-drain"),
            ))
            .unwrap();

        // A v1/v2-compatible active record has a positive CAS version after
        // persistence but retains the legacy `unconfigured` restart state.
        // It must not be mistaken for the known pre-spawn window.
        let mut legacy = supervisor.table.read(&slot.session_id).unwrap();
        legacy.restart_state = "unconfigured".to_owned();
        supervisor.table.compare_and_set(legacy).unwrap();

        let options = ProviderDrainOptions::new(Duration::from_millis(20))
            .unwrap()
            .with_poll_interval(Duration::from_millis(1))
            .unwrap();
        let first = supervisor.drain(options).unwrap_err();
        assert_eq!(first.kind(), eva_core::ErrorKind::Timeout);
        assert!(first
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "active_provider_count" && value == "1"));
        assert!(supervisor.table.read(&slot.session_id).unwrap().active);

        // A failed drain is terminal for this supervisor generation. The
        // cached error prevents a later call from claiming success after a
        // partial cleanup.
        let repeated = supervisor.drain(options).unwrap_err();
        assert_eq!(repeated, first);
        assert!(supervisor.is_draining());
    }

    #[test]
    fn supervisor_drain_never_releases_a_successor_reservation() {
        let root = std::env::temp_dir().join(format!(
            "eva-adapter-drain-successor-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let table = FileSystemProviderAdmissionTable::new(&root).unwrap();
        let mut handle = handle();
        handle.max_concurrency = Some(1);
        let mut supervisor = InMemoryProviderSupervisor::new();
        supervisor.admission_table = Some(table.clone());
        let slot = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-successor-drain"),
            ))
            .unwrap();
        let original_reservation_id = slot.admission_reservation_id.clone().unwrap();
        let original = table
            .snapshot(&handle.id, now_ms())
            .unwrap()
            .reservations
            .into_iter()
            .find(|reservation| reservation.reservation_id == original_reservation_id)
            .unwrap();
        let successor = table
            .reserve(
                &handle.id,
                1,
                &slot.session_id,
                original.expires_at_ms,
                eva_storage::DEFAULT_RESERVATION_TTL_MS,
            )
            .unwrap();
        assert_ne!(successor.reservation_id, original_reservation_id);

        let options = ProviderDrainOptions::new(Duration::from_millis(20))
            .unwrap()
            .with_poll_interval(Duration::from_millis(1))
            .unwrap();
        let first = supervisor.drain(options).unwrap_err();
        assert_eq!(first.kind(), eva_core::ErrorKind::Conflict);
        assert!(supervisor.table.read(&slot.session_id).unwrap().active);
        assert_eq!(
            table.snapshot(&handle.id, now_ms()).unwrap().reservations,
            vec![successor.clone()]
        );

        // The failed result is cached, and a retry cannot consume the
        // successor reservation under the old session ID.
        assert_eq!(supervisor.drain(options).unwrap_err(), first);
        assert_eq!(
            table.snapshot(&handle.id, now_ms()).unwrap().reservations,
            vec![successor]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn supervisor_reconciles_expired_release_intent_without_touching_successor() {
        let root = std::env::temp_dir().join(format!(
            "eva-adapter-drain-pending-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let table = FileSystemProviderAdmissionTable::new(&root).unwrap();
        let mut handle = handle();
        handle.max_concurrency = Some(1);
        let mut supervisor = InMemoryProviderSupervisor::new();
        supervisor.admission_table = Some(table.clone());
        let slot = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-pending-release"),
            ))
            .unwrap();
        let old_reservation_id = slot.admission_reservation_id.clone().unwrap();
        let old_reservation = table
            .snapshot(&handle.id, now_ms())
            .unwrap()
            .reservations
            .into_iter()
            .find(|reservation| reservation.reservation_id == old_reservation_id)
            .unwrap();

        // Simulate a crash after the provider snapshot CAS but before the
        // admission release. Include an older unresolved intent to prove one
        // reconciliation pass drains the complete durable backlog rather than
        // only the newest marker.
        let mut retired = supervisor.table.read(&slot.session_id).unwrap();
        let older_reservation_id = "expired-before-current";
        retired.audit.push(format!(
            "{PROVIDER_ADMISSION_RELEASE_PENDING_AUDIT_PREFIX}{older_reservation_id}"
        ));
        retired.audit.push(format!(
            "{PROVIDER_ADMISSION_RELEASE_PENDING_AUDIT_PREFIX}{old_reservation_id}"
        ));
        retired
            .release("interrupted", Some("simulated drain crash".to_owned()))
            .unwrap();
        supervisor.table.compare_and_set(retired).unwrap();

        // Expiry allows a successor reservation with the same session ID to
        // appear. Reconciliation must preserve it and only resolve the old
        // release intent.
        let successor = table
            .reserve(
                &handle.id,
                1,
                &slot.session_id,
                old_reservation.expires_at_ms,
                eva_storage::DEFAULT_RESERVATION_TTL_MS,
            )
            .unwrap();
        assert_ne!(successor.reservation_id, old_reservation_id);

        let report = supervisor
            .drain(
                // This success-path test performs several durable filesystem
                // operations; keep its budget above CI scheduler jitter.
                ProviderDrainOptions::new(Duration::from_secs(1))
                    .unwrap()
                    .with_poll_interval(Duration::from_millis(1))
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(report.phase, "drained");
        let reconciled = supervisor.table.read(&slot.session_id).unwrap();
        assert!(pending_release_id(&reconciled).is_none());
        assert!(reconciled.audit.iter().any(|entry| {
            entry == &format!("provider.admission:successor_preserved:{old_reservation_id}")
        }));
        assert!(reconciled.audit.iter().any(|entry| {
            entry
                == &format!(
                    "{PROVIDER_ADMISSION_RELEASE_RESOLVED_AUDIT_PREFIX}{old_reservation_id}"
                )
        }));
        assert!(reconciled.audit.iter().any(|entry| {
            entry
                == &format!(
                    "{PROVIDER_ADMISSION_RELEASE_RESOLVED_AUDIT_PREFIX}{older_reservation_id}"
                )
        }));
        assert_eq!(
            table.snapshot(&handle.id, now_ms()).unwrap().reservations,
            vec![successor]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// 验证 `supervisor_rejects_concurrency_limit_without_new_slot` 场景下的预期行为。
    #[test]
    fn supervisor_rejects_concurrency_limit_without_new_slot() {
        let mut handle = handle();
        handle.max_concurrency = Some(1);
        let mut supervisor = InMemoryProviderSupervisor::new();

        let first = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-concurrency-a"),
            ))
            .unwrap();
        let error = supervisor
            .acquire(
                ProviderExecutionRequest::from_handle(
                    &handle,
                    &invocation("req-supervisor-concurrency-b"),
                )
                .with_retry_backoff_ms(Some(1000)),
            )
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Unavailable);
        assert!(error.is_retryable());
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("provider_concurrency_limited")
        );
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "retry_after_ms" && value == "1000"));
        assert_eq!(supervisor.active_for_adapter(&handle.id).unwrap().len(), 1);

        supervisor
            .complete(&first, ProviderExecutionOutcome::completed("completed"))
            .unwrap();
    }

    /// 验证 `supervisor_rejects_rate_limit_without_starting_new_process` 场景下的预期行为。
    #[test]
    fn supervisor_rejects_rate_limit_without_starting_new_process() {
        let mut handle = handle();
        handle.rate_limit = Some(AdapterRateLimit {
            max_requests: 1,
            window_ms: 60_000,
        });
        let mut supervisor = InMemoryProviderSupervisor::new();

        let first = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-rate-a"),
            ))
            .unwrap();
        supervisor
            .complete(&first, ProviderExecutionOutcome::completed("completed"))
            .unwrap();
        let error = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-rate-b"),
            ))
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Unavailable);
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("provider_rate_limited")
        );
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, _)| key == "retry_after_ms"));
        assert_eq!(supervisor.processes().unwrap().len(), 1);
    }

    /// 验证 `supervisor_opens_circuit_and_blocks_new_processes` 场景下的预期行为。
    #[test]
    fn supervisor_opens_circuit_and_blocks_new_processes() {
        let mut handle = handle();
        handle.circuit_breaker = Some(AdapterCircuitBreaker {
            failure_threshold: 1,
            recovery_window_ms: 60_000,
        });
        let mut supervisor = InMemoryProviderSupervisor::new();

        let first = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-circuit-a"),
            ))
            .unwrap();
        let snapshot = supervisor
            .complete(
                &first,
                ProviderExecutionOutcome::failed(&EvaError::unavailable("provider failed")),
            )
            .unwrap();
        let error = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-circuit-b"),
            ))
            .unwrap_err();

        assert_eq!(snapshot.health, "circuit_open");
        assert!(snapshot
            .audit
            .iter()
            .any(|entry| entry == "provider.circuit:opened"));
        assert_eq!(error.kind(), eva_core::ErrorKind::Unavailable);
        assert_eq!(
            error.provider_code().map(|code| code.as_str()),
            Some("provider_circuit_open")
        );
        assert_eq!(supervisor.processes().unwrap().len(), 1);
    }

    /// 验证 `supervisor_allows_half_open_probe_after_recovery_window` 场景下的预期行为。
    #[test]
    fn supervisor_allows_half_open_probe_after_recovery_window() {
        let mut handle = handle();
        handle.circuit_breaker = Some(AdapterCircuitBreaker {
            failure_threshold: 1,
            recovery_window_ms: 0,
        });
        let mut supervisor = InMemoryProviderSupervisor::new();

        let first = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-half-open-a"),
            ))
            .unwrap();
        supervisor
            .complete(
                &first,
                ProviderExecutionOutcome::failed(&EvaError::unavailable("provider failed")),
            )
            .unwrap();
        let probe = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &handle,
                &invocation("req-supervisor-half-open-b"),
            ))
            .unwrap();
        let snapshot = supervisor
            .complete(&probe, ProviderExecutionOutcome::completed("completed"))
            .unwrap();

        assert!(probe.half_open_probe);
        assert_eq!(snapshot.health, "completed");
        assert!(snapshot
            .audit
            .iter()
            .any(|entry| entry == "provider.circuit:half_open_probe"));
        assert!(snapshot
            .audit
            .iter()
            .any(|entry| entry == "provider.circuit:closed"));
    }

    /// 验证 `skill_start_command_uses_matching_fallback_args` 场景下的预期行为。
    #[test]
    fn skill_start_command_uses_matching_fallback_args() {
        let mut handle = handle();
        handle.transport = AdapterTransport::Skill;
        handle.command = Some("skill-fallback".to_owned());
        handle.args = vec!["--fallback".to_owned()];
        handle.skill_runner_command = None;
        handle.skill_runner_args = vec!["--runner".to_owned()];

        let request = ProviderExecutionRequest::from_handle(
            &handle,
            &AdapterInvocation::new(
                RequestId::parse("req-supervisor-2").unwrap(),
                CapabilityName::parse("repo.analyze").unwrap(),
            ),
        );

        assert_eq!(request.start_command, "skill-fallback --fallback");
    }

    /// 验证 `credential_scope_rejects_cross_provider_reuse` 场景下的预期行为。
    #[test]
    fn credential_scope_rejects_cross_provider_reuse() {
        let scope = ProviderCredentialScope::new_for_session(
            "session-stdio-req",
            AdapterId::parse("stdio-test").unwrap(),
            RequestId::parse("req-supervisor-credentials").unwrap(),
            CapabilityName::parse("repo.analyze").unwrap(),
        );

        let error = scope
            .ensure_matches(
                &AdapterId::parse("other-provider").unwrap(),
                &RequestId::parse("req-supervisor-credentials").unwrap(),
                &CapabilityName::parse("repo.analyze").unwrap(),
            )
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::PermissionDenied);
        assert!(!scope
            .audit_entries()
            .iter()
            .any(|entry| entry.contains("eva-provider-session:")));
    }

    /// 验证 `credential_scope_injects_token_without_exposing_it_in_audit` 场景下的预期行为。
    #[test]
    fn credential_scope_injects_token_without_exposing_it_in_audit() {
        let scope = ProviderCredentialScope::new_for_session(
            "session-stdio-req",
            AdapterId::parse("stdio-test").unwrap(),
            RequestId::parse("req-supervisor-credentials").unwrap(),
            CapabilityName::parse("repo.analyze").unwrap(),
        );
        let mut env = BTreeMap::new();
        let mut headers = BTreeMap::new();

        scope.apply_env(&mut env);
        scope.apply_headers(&mut headers);

        assert_eq!(
            env.get(PROVIDER_SESSION_ID_ENV).map(String::as_str),
            Some("session-stdio-req")
        );
        assert!(env
            .get(PROVIDER_SESSION_TOKEN_ENV)
            .unwrap()
            .starts_with("eva-provider-session:"));
        assert!(headers.contains_key(PROVIDER_SESSION_TOKEN_HEADER));
        assert!(scope
            .audit_entries()
            .contains(&"credential.session_token:redacted".to_owned()));
    }

    /// 验证 `provider_session_token_redaction_catches_prefixed_values` 场景下的预期行为。
    #[test]
    fn provider_session_token_redaction_catches_prefixed_values() {
        let redacted = redact_provider_session_tokens(
            "before eva-provider-session:session-1:fnv64:abc123 after",
        );

        assert_eq!(redacted, "before [REDACTED] after");
    }

    #[test]
    fn manifest_digest_binds_provider_restart_identity_env_and_vault_refs() {
        let mut baseline = handle();
        baseline.credential_env = vec!["API_TOKEN".to_owned()];
        let baseline_digest =
            ProviderExecutionRequest::from_handle(&baseline, &invocation("req-provider-digest"))
                .manifest_digest;

        let mut restart_changed = baseline.clone();
        restart_changed.provider.restart = eva_config::ProviderRestartConfig {
            mode: eva_config::ProviderRestartMode::OnFailure,
            max_attempts: 2,
            backoff_ms: 100,
        };
        assert_ne!(
            baseline_digest,
            ProviderExecutionRequest::from_handle(
                &restart_changed,
                &invocation("req-provider-digest"),
            )
            .manifest_digest
        );

        let mut identity_changed = baseline.clone();
        identity_changed.provider.run_as = eva_config::ProviderRunAsIdentity::Unix {
            uid: 1000,
            gid: 1001,
        };
        assert_ne!(
            baseline_digest,
            ProviderExecutionRequest::from_handle(
                &identity_changed,
                &invocation("req-provider-digest"),
            )
            .manifest_digest
        );

        let mut env_changed = baseline.clone();
        env_changed.credential_env.push("SECOND_TOKEN".to_owned());
        assert_ne!(
            baseline_digest,
            ProviderExecutionRequest::from_handle(
                &env_changed,
                &invocation("req-provider-digest"),
            )
            .manifest_digest
        );

        let mut vault_changed = baseline;
        vault_changed
            .provider
            .vault_secrets
            .push(eva_config::ProviderVaultSecretRef {
                env: "API_TOKEN".to_owned(),
                secret_ref: "vault://providers/digest/token#value".to_owned(),
            });
        let vault_digest = ProviderExecutionRequest::from_handle(
            &vault_changed,
            &invocation("req-provider-digest"),
        )
        .manifest_digest;
        assert_ne!(baseline_digest, vault_digest);
    }

    #[test]
    fn manifest_digest_binds_mcp_http_policy_without_exposing_references() {
        let digest = |handle: &AdapterHandle| {
            ProviderExecutionRequest::from_handle(handle, &invocation("req-mcp-policy-digest"))
                .manifest_digest
        };
        let mut baseline = handle();
        baseline.transport = AdapterTransport::Mcp;
        baseline.mcp_server_transport = Some("streamable_http".to_owned());
        baseline.endpoint = Some("https://example.test/mcp".to_owned());
        baseline.mcp_http_config = Some(
            eva_mcp::McpStreamableHttpConfig::from_parts(
                "https://example.test/mcp",
                ["system"],
                Some(
                    eva_mcp::McpClientAuthConfig::new(
                        "env:CLIENT_CERT",
                        "vault://providers/mcp/client-key#value",
                    )
                    .unwrap(),
                ),
                eva_mcp::McpRedirectPolicy::Deny,
                ["https://example.test"],
            )
            .unwrap(),
        );
        baseline
            .headers
            .insert("Authorization".to_owned(), "env:MCP_TOKEN".to_owned());
        let baseline_digest = digest(&baseline);

        let mut endpoint_changed = baseline.clone();
        endpoint_changed.endpoint = Some("https://example.test/other".to_owned());
        endpoint_changed.mcp_http_config.as_mut().unwrap().endpoint =
            "https://example.test/other".to_owned();
        assert_ne!(baseline_digest, digest(&endpoint_changed));

        let mut trust_changed = baseline.clone();
        trust_changed
            .mcp_http_config
            .as_mut()
            .unwrap()
            .trust_roots
            .insert("pem:sha256:other-root".to_owned());
        assert_ne!(baseline_digest, digest(&trust_changed));

        let mut redirect_changed = baseline.clone();
        redirect_changed
            .mcp_http_config
            .as_mut()
            .unwrap()
            .redirect_policy = eva_mcp::McpRedirectPolicy::SameOrigin { max_hops: 2 };
        assert_ne!(baseline_digest, digest(&redirect_changed));

        let mut auth_changed = baseline.clone();
        auth_changed
            .mcp_http_config
            .as_mut()
            .unwrap()
            .client_auth
            .as_mut()
            .unwrap()
            .private_key_ref = "vault://providers/mcp/rotated-key#value".to_owned();
        assert_ne!(baseline_digest, digest(&auth_changed));

        let mut header_changed = baseline.clone();
        header_changed
            .headers
            .insert("Authorization".to_owned(), "env:ROTATED_TOKEN".to_owned());
        assert_ne!(baseline_digest, digest(&header_changed));
        assert!(!baseline_digest.contains("CLIENT_CERT"));
        assert!(!baseline_digest.contains("vault://"));
    }

    #[test]
    fn provider_audit_contains_only_identity_kind_and_vault_count() {
        let mut configured = handle();
        configured.provider.run_as = eva_config::ProviderRunAsIdentity::Windows {
            account: "SecretAccountName".to_owned(),
        };
        configured.provider.vault_secrets = vec![eva_config::ProviderVaultSecretRef {
            env: "API_TOKEN".to_owned(),
            secret_ref: "vault://providers/audit/token#value".to_owned(),
        }];
        let mut supervisor = InMemoryProviderSupervisor::new();
        let slot = supervisor
            .acquire(ProviderExecutionRequest::from_handle(
                &configured,
                &invocation("req-provider-audit"),
            ))
            .unwrap();
        let snapshot = supervisor.processes().unwrap().pop().unwrap();

        assert!(snapshot
            .audit
            .iter()
            .any(|entry| entry == "provider.run_as.kind:windows"));
        assert!(snapshot
            .audit
            .iter()
            .any(|entry| entry == "provider.vault_secret_refs:1"));
        assert!(!snapshot
            .audit
            .iter()
            .any(|entry| entry.contains("SecretAccountName") || entry.contains("vault://")));
        assert!(!snapshot.manifest_digest.contains("SecretAccountName"));
        assert!(!snapshot
            .manifest_digest
            .contains("vault://providers/audit/token"));

        supervisor
            .complete(&slot, ProviderExecutionOutcome::completed("completed"))
            .unwrap();
    }

    #[test]
    fn manifest_digest_canonicalizes_unordered_provider_sets() {
        let mut first = handle();
        first.credential_env = vec!["SECOND_TOKEN".to_owned(), "API_TOKEN".to_owned()];
        first.provider.vault_secrets = vec![
            eva_config::ProviderVaultSecretRef {
                env: "SECOND_TOKEN".to_owned(),
                secret_ref: "vault://providers/z/token".to_owned(),
            },
            eva_config::ProviderVaultSecretRef {
                env: "API_TOKEN".to_owned(),
                secret_ref: "vault://providers/a/token".to_owned(),
            },
        ];
        let mut second = first.clone();
        second.credential_env.reverse();
        second.provider.vault_secrets.reverse();

        let first_digest =
            ProviderExecutionRequest::from_handle(&first, &invocation("req-provider-canonical-a"))
                .manifest_digest;
        let second_digest =
            ProviderExecutionRequest::from_handle(&second, &invocation("req-provider-canonical-b"))
                .manifest_digest;

        assert_eq!(first_digest, second_digest);
    }

    #[test]
    fn manifest_digest_preserves_native_argv_boundaries() {
        let digest = |handle: &AdapterHandle, request_id: &str| {
            ProviderExecutionRequest::from_handle(handle, &invocation(request_id)).manifest_digest
        };

        let mut stdio_single = handle();
        stdio_single.args = vec!["--scope workspace".to_owned()];
        let mut stdio_split = stdio_single.clone();
        stdio_split.args = vec!["--scope".to_owned(), "workspace".to_owned()];
        assert_ne!(
            digest(&stdio_single, "req-provider-stdio-single"),
            digest(&stdio_split, "req-provider-stdio-split")
        );

        let mut mcp_single = handle();
        mcp_single.transport = AdapterTransport::Mcp;
        mcp_single.mcp_command = Some("provider".to_owned());
        mcp_single.mcp_args = vec!["--scope workspace".to_owned()];
        let mut mcp_split = mcp_single.clone();
        mcp_split.mcp_args = vec!["--scope".to_owned(), "workspace".to_owned()];
        assert_ne!(
            digest(&mcp_single, "req-provider-mcp-single"),
            digest(&mcp_split, "req-provider-mcp-split")
        );

        let mut skill_single = handle();
        skill_single.transport = AdapterTransport::Skill;
        skill_single.skill_runner_command = Some("provider".to_owned());
        skill_single.skill_runner_args = vec!["--scope workspace".to_owned()];
        let mut skill_split = skill_single.clone();
        skill_split.skill_runner_args = vec!["--scope".to_owned(), "workspace".to_owned()];
        assert_ne!(
            digest(&skill_single, "req-provider-skill-single"),
            digest(&skill_split, "req-provider-skill-split")
        );
    }
}
