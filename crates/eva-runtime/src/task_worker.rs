//! Durable task handler lookup and payload integrity boundary.

use crate::{TaskArtifactRef, TaskEnvelope, TaskInput, TaskKind};
use eva_core::{ErrorKind, EvaError, RequestId};
use eva_storage::{
    artifact_store::sha256_digest, ArtifactRecord, ArtifactStore, FileSystemArtifactStore,
    FileSystemTaskStateStore, InMemoryArtifactStore, TaskAttemptOutcome, TaskExecutionClaim,
    TaskStateStore,
};
use std::collections::BTreeMap;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// This module owns handler registration and one synchronous dispatch boundary.
pub const RESPONSIBILITY: &str = "task handler registration and payload integrity dispatch";

/// Stable human-readable message for a syntactically valid but unregistered task kind.
pub const TASK_HANDLER_NOT_REGISTERED_MESSAGE: &str = "task handler is not registered";

/// Default upper bound for one artifact-backed task input read by the daemon.
pub const DEFAULT_TASK_ARTIFACT_INPUT_LIMIT_BYTES: usize = 16 * 1024 * 1024;

/// Idle wait between durable task scans. Explicit notifications wake the worker earlier.
pub const DEFAULT_TASK_WORKER_POLL_INTERVAL_MS: u64 = 25;

const TASK_HANDLER_PANIC_MESSAGE: &str = "task handler panicked";

/// Read-only cooperative cancellation signal passed to a task handler.
#[derive(Clone, Default)]
pub struct TaskCancellationView {
    requested: Arc<AtomicBool>,
}

impl TaskCancellationView {
    /// Returns whether durable cancellation or daemon shutdown requested cooperative stop.
    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    fn request(&self) {
        self.requested.store(true, Ordering::Release);
    }
}

impl fmt::Debug for TaskCancellationView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskCancellationView")
            .field("requested", &self.is_requested())
            .finish()
    }
}

/// A registered task implementation. Lifecycle ownership and retry policy remain with the worker.
pub trait TaskHandler: Send + Sync {
    /// Handles one immutable envelope with the exact resolved input bytes.
    fn handle(&self, invocation: &TaskHandlerInvocation<'_>)
        -> Result<TaskHandlerResult, EvaError>;
}

impl<F> TaskHandler for F
where
    F: Fn(&TaskHandlerInvocation<'_>) -> Result<TaskHandlerResult, EvaError> + Send + Sync,
{
    fn handle(
        &self,
        invocation: &TaskHandlerInvocation<'_>,
    ) -> Result<TaskHandlerResult, EvaError> {
        self(invocation)
    }
}

/// Strict artifact lookup used immediately before a handler receives referenced bytes.
pub trait TaskArtifactResolver: Send + Sync {
    /// Returns the artifact record, `None` when absent, or a structured integrity/I/O error.
    fn resolve_task_artifact(
        &self,
        reference: &TaskArtifactRef,
    ) -> Result<Option<ArtifactRecord>, EvaError>;
}

/// Filesystem resolver that never performs an unbounded task-input read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemTaskArtifactResolver {
    store: FileSystemArtifactStore,
    max_size_bytes: usize,
}

impl FileSystemTaskArtifactResolver {
    /// Creates a resolver with an explicit inclusive byte limit.
    pub fn new(root: impl AsRef<Path>, max_size_bytes: usize) -> Self {
        Self {
            store: FileSystemArtifactStore::new(root),
            max_size_bytes,
        }
    }

    /// Creates a resolver with the daemon's conservative default input limit.
    pub fn with_default_limit(root: impl AsRef<Path>) -> Self {
        Self::new(root, DEFAULT_TASK_ARTIFACT_INPUT_LIMIT_BYTES)
    }

    /// Returns the artifact root without resolving or canonicalizing it again.
    pub fn root(&self) -> &Path {
        self.store.root()
    }

    /// Returns the inclusive read limit applied before handler dispatch.
    pub fn max_size_bytes(&self) -> usize {
        self.max_size_bytes
    }
}

impl TaskArtifactResolver for FileSystemTaskArtifactResolver {
    fn resolve_task_artifact(
        &self,
        reference: &TaskArtifactRef,
    ) -> Result<Option<ArtifactRecord>, EvaError> {
        self.store
            .try_get_bytes_with_limit(reference.key(), self.max_size_bytes)
    }
}

impl TaskArtifactResolver for InMemoryArtifactStore {
    fn resolve_task_artifact(
        &self,
        reference: &TaskArtifactRef,
    ) -> Result<Option<ArtifactRecord>, EvaError> {
        Ok(ArtifactStore::get_bytes(self, reference.key()))
    }
}

/// Immutable view passed to one handler invocation.
pub struct TaskHandlerInvocation<'a> {
    task_id: &'a RequestId,
    envelope: &'a TaskEnvelope,
    payload: &'a [u8],
    payload_digest: &'a str,
    attempt: usize,
    deadline_at_ms: Option<u128>,
    cancellation: &'a TaskCancellationView,
}

impl<'a> TaskHandlerInvocation<'a> {
    /// Returns the durable task identity selected by the worker.
    pub fn task_id(&self) -> &'a RequestId {
        self.task_id
    }

    /// Returns the persisted business envelope without permitting mutation.
    pub fn envelope(&self) -> &'a TaskEnvelope {
        self.envelope
    }

    /// Returns the original inline bytes or the verified artifact bytes.
    pub fn payload(&self) -> &'a [u8] {
        self.payload
    }

    /// Returns the canonical digest independently verified at this dispatch boundary.
    pub fn payload_digest(&self) -> &'a str {
        self.payload_digest
    }

    /// Returns the durable one-based attempt number, or zero for direct registry dispatch.
    pub const fn attempt(&self) -> usize {
        self.attempt
    }

    /// Returns the persisted attempt deadline, when configured.
    pub const fn deadline_at_ms(&self) -> Option<u128> {
        self.deadline_at_ms
    }

    /// Returns the cooperative cancellation view without exposing its fencing token.
    pub fn cancellation(&self) -> &'a TaskCancellationView {
        self.cancellation
    }
}

impl fmt::Debug for TaskHandlerInvocation<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskHandlerInvocation")
            .field("task_id", &self.task_id.as_str())
            .field("task_kind", &self.envelope.kind().as_str())
            .field("agent_id", &self.envelope.agent_id().as_str())
            .field("payload", &"<redacted>")
            .field("payload_size_bytes", &self.payload.len())
            .field("payload_digest", &self.payload_digest)
            .field("attempt", &self.attempt)
            .field("deadline_at_ms", &self.deadline_at_ms)
            .field("cancellation", &self.cancellation)
            .finish()
    }
}

/// Opaque handler output with a canonical digest for later terminal-state or ledger use.
#[derive(Clone, PartialEq, Eq)]
pub struct TaskHandlerResult {
    bytes: Vec<u8>,
    digest: String,
}

impl TaskHandlerResult {
    /// Creates a result and binds its exact bytes to SHA-256.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        let bytes = bytes.into();
        let digest = sha256_digest(&bytes);
        Self { bytes, digest }
    }

    /// Returns the exact result bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consumes the result and returns its exact bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Returns the canonical result digest.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Returns the result size without exposing its bytes.
    pub fn size_bytes(&self) -> usize {
        self.bytes.len()
    }
}

impl fmt::Debug for TaskHandlerResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskHandlerResult")
            .field("bytes", &"<redacted>")
            .field("size_bytes", &self.bytes.len())
            .field("digest", &self.digest)
            .finish()
    }
}

/// Deterministic task-kind registry shared by daemon-owned workers.
#[derive(Default)]
pub struct TaskHandlerRegistry {
    handlers: BTreeMap<TaskKind, Arc<dyn TaskHandler>>,
}

impl TaskHandlerRegistry {
    /// Creates an empty registry. Registration is explicit and duplicate kinds fail closed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates the production-safe built-in registry.
    pub fn with_runtime_defaults() -> Result<Self, EvaError> {
        let mut registry = Self::new();
        registry.register(TaskKind::parse("runtime.echo")?, runtime_echo_task_handler)?;
        Ok(registry)
    }

    /// Registers one handler without replacing an existing task kind.
    pub fn register<H>(&mut self, kind: TaskKind, handler: H) -> Result<(), EvaError>
    where
        H: TaskHandler + 'static,
    {
        if self.handlers.contains_key(&kind) {
            return Err(
                EvaError::conflict("task handler kind is already registered")
                    .with_context("task_kind", kind.as_str()),
            );
        }
        self.handlers.insert(kind, Arc::new(handler));
        Ok(())
    }

    /// Returns whether this exact validated kind has a handler.
    pub fn contains(&self, kind: &TaskKind) -> bool {
        self.handlers.contains_key(kind)
    }

    /// Returns the number of distinct registered kinds.
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    /// Returns whether no handlers are registered.
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    /// Returns task kinds in deterministic lexical order.
    pub fn registered_kinds(&self) -> Vec<&str> {
        self.handlers.keys().map(TaskKind::as_str).collect()
    }

    /// Resolves and revalidates input, then invokes exactly one registered handler.
    ///
    /// Lookup deliberately precedes artifact access: an unknown kind cannot trigger input I/O and
    /// always returns the same non-retryable `not_found` failure. A successful return is therefore
    /// the only boundary a later worker may treat as executed.
    pub fn dispatch(
        &self,
        task_id: &RequestId,
        envelope: &TaskEnvelope,
        artifacts: &dyn TaskArtifactResolver,
    ) -> Result<TaskHandlerResult, EvaError> {
        let cancellation = TaskCancellationView::default();
        self.dispatch_attempt(task_id, envelope, artifacts, 0, None, &cancellation)
    }

    /// Dispatches one durably claimed attempt with its deadline and cancellation view.
    pub fn dispatch_attempt(
        &self,
        task_id: &RequestId,
        envelope: &TaskEnvelope,
        artifacts: &dyn TaskArtifactResolver,
        attempt: usize,
        deadline_at_ms: Option<u128>,
        cancellation: &TaskCancellationView,
    ) -> Result<TaskHandlerResult, EvaError> {
        let handler = self.handlers.get(envelope.kind()).ok_or_else(|| {
            EvaError::not_found(TASK_HANDLER_NOT_REGISTERED_MESSAGE)
                .with_context("task_id", task_id.as_str())
                .with_context("task_kind", envelope.kind().as_str())
                .with_context("agent_id", envelope.agent_id().as_str())
        })?;
        let (payload, payload_digest) =
            resolve_task_payload(envelope, artifacts).map_err(|error| {
                error
                    .with_context("task_id", task_id.as_str())
                    .with_context("agent_id", envelope.agent_id().as_str())
            })?;
        let invocation = TaskHandlerInvocation {
            task_id,
            envelope,
            payload: &payload,
            payload_digest: &payload_digest,
            attempt,
            deadline_at_ms,
            cancellation,
        };
        handler.handle(&invocation).map_err(|error| {
            error
                .with_context("task_id", task_id.as_str())
                .with_context("task_kind", envelope.kind().as_str())
                .with_context("agent_id", envelope.agent_id().as_str())
        })
    }
}

#[derive(Clone)]
struct ActiveTaskAttempt {
    cancel_token: String,
    cancellation: TaskCancellationView,
}

struct TaskWorkerShared {
    stop_requested: AtomicBool,
    claims_enabled: AtomicBool,
    wait_lock: Mutex<()>,
    wake: Condvar,
    active: Mutex<BTreeMap<String, ActiveTaskAttempt>>,
    fatal_error: Mutex<Option<EvaError>>,
}

impl TaskWorkerShared {
    fn new() -> Self {
        Self {
            stop_requested: AtomicBool::new(false),
            claims_enabled: AtomicBool::new(false),
            wait_lock: Mutex::new(()),
            wake: Condvar::new(),
            active: Mutex::new(BTreeMap::new()),
            fatal_error: Mutex::new(None),
        }
    }

    fn should_stop(&self) -> bool {
        self.stop_requested.load(Ordering::Acquire)
    }

    fn claims_enabled(&self) -> bool {
        self.claims_enabled.load(Ordering::Acquire)
    }

    fn set_fatal_error(&self, error: EvaError) {
        let mut fatal = self
            .fatal_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if fatal.is_none() {
            *fatal = Some(error);
        }
        self.wake.notify_all();
    }

    fn fatal_error(&self) -> Option<EvaError> {
        self.fatal_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn register_attempt(
        &self,
        task_id: &str,
        cancel_token: &str,
        cancellation: TaskCancellationView,
    ) -> Result<(), EvaError> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if active.contains_key(task_id) {
            return Err(
                EvaError::internal("task worker registered the same task more than once")
                    .with_context("task_id", task_id),
            );
        }
        active.insert(
            task_id.to_owned(),
            ActiveTaskAttempt {
                cancel_token: cancel_token.to_owned(),
                cancellation,
            },
        );
        Ok(())
    }

    fn unregister_attempt(&self, task_id: &str, cancel_token: &str) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if active
            .get(task_id)
            .is_some_and(|attempt| attempt.cancel_token == cancel_token)
        {
            active.remove(task_id);
        }
    }

    fn signal_cancellation(&self, task_id: &str, cancel_token: Option<&str>) -> bool {
        let active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(attempt) = active.get(task_id) else {
            return false;
        };
        if cancel_token != Some(attempt.cancel_token.as_str()) {
            return false;
        }
        attempt.cancellation.request();
        true
    }

    fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::Release);
        let active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for attempt in active.values() {
            attempt.cancellation.request();
        }
        drop(active);
        self.wake.notify_all();
    }

    fn wait_for_work(&self) {
        let guard = self
            .wait_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !self.should_stop() {
            let _ = self.wake.wait_timeout(
                guard,
                Duration::from_millis(DEFAULT_TASK_WORKER_POLL_INTERVAL_MS),
            );
        }
    }
}

/// Daemon-owned durable task claim and execution thread.
pub struct TaskWorkerRuntime {
    shared: Arc<TaskWorkerShared>,
    join: Option<JoinHandle<()>>,
}

impl TaskWorkerRuntime {
    /// Starts one immediately active worker. Daemon startup should use `start_paused` instead.
    pub fn start(
        store: FileSystemTaskStateStore,
        registry: Arc<TaskHandlerRegistry>,
        artifacts: Arc<dyn TaskArtifactResolver>,
        execution_owner: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let worker = Self::start_paused(store, registry, artifacts, execution_owner)?;
        worker.activate();
        Ok(worker)
    }

    /// Creates the worker thread while keeping durable claims disabled until ready publication.
    pub fn start_paused(
        store: FileSystemTaskStateStore,
        registry: Arc<TaskHandlerRegistry>,
        artifacts: Arc<dyn TaskArtifactResolver>,
        execution_owner: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let execution_owner = execution_owner.into();
        if execution_owner.is_empty()
            || execution_owner.trim() != execution_owner
            || execution_owner.len() > 512
            || execution_owner.chars().any(char::is_control)
        {
            return Err(EvaError::invalid_argument(
                "task worker execution owner is invalid",
            ));
        }
        let shared = Arc::new(TaskWorkerShared::new());
        let thread_shared = Arc::clone(&shared);
        let join = thread::Builder::new()
            .name("eva-task-worker".to_owned())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    run_task_worker_loop(
                        store,
                        registry,
                        artifacts,
                        execution_owner,
                        Arc::clone(&thread_shared),
                    )
                }));
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => thread_shared.set_fatal_error(error),
                    Err(_) => thread_shared.set_fatal_error(EvaError::internal(
                        "task worker thread panicked outside handler isolation",
                    )),
                }
            })
            .map_err(|error| {
                EvaError::internal("failed to start task worker thread")
                    .with_context("io_error", error.to_string())
            })?;
        Ok(Self {
            shared,
            join: Some(join),
        })
    }

    /// Opens the one-way readiness gate and wakes the worker to begin durable claims.
    pub fn activate(&self) {
        self.shared.claims_enabled.store(true, Ordering::Release);
        self.shared.wake.notify_all();
    }

    /// Wakes the worker after a durable submission without changing task state.
    pub fn notify_new_work(&self) {
        self.shared.wake.notify_all();
    }

    /// Signals only the active attempt whose durable cancel token still matches.
    pub fn signal_cancellation(&self, task_id: &str, cancel_token: Option<&str>) -> bool {
        self.shared.signal_cancellation(task_id, cancel_token)
    }

    /// Returns a fatal worker error or detects an unexpected worker exit.
    pub fn check_health(&self) -> Result<(), EvaError> {
        if let Some(error) = self.shared.fatal_error() {
            return Err(error);
        }
        if !self.shared.should_stop()
            && self
                .join
                .as_ref()
                .is_some_and(std::thread::JoinHandle::is_finished)
        {
            return Err(EvaError::internal("task worker stopped unexpectedly"));
        }
        Ok(())
    }

    /// Closes the claim gate and signals active handlers before shutdown state is published.
    pub fn begin_shutdown(&self) {
        self.shared.request_stop();
    }

    /// Stops new claims, signals cooperative cancellation, and joins before writer release.
    pub fn stop_and_join(&mut self) -> Result<(), EvaError> {
        self.shared.request_stop();
        if let Some(join) = self.join.take() {
            join.join()
                .map_err(|_| EvaError::internal("failed to join task worker thread"))?;
        }
        match self.shared.fatal_error() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl fmt::Debug for TaskWorkerRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let active_tasks = self
            .shared
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len();
        formatter
            .debug_struct("TaskWorkerRuntime")
            .field("stop_requested", &self.shared.should_stop())
            .field("claims_enabled", &self.shared.claims_enabled())
            .field("active_tasks", &active_tasks)
            .field("healthy", &self.shared.fatal_error().is_none())
            .finish()
    }
}

impl Drop for TaskWorkerRuntime {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

fn run_task_worker_loop(
    mut store: FileSystemTaskStateStore,
    registry: Arc<TaskHandlerRegistry>,
    artifacts: Arc<dyn TaskArtifactResolver>,
    execution_owner: String,
    shared: Arc<TaskWorkerShared>,
) -> Result<(), EvaError> {
    loop {
        if shared.should_stop() {
            return Ok(());
        }
        if !shared.claims_enabled() {
            shared.wait_for_work();
            continue;
        }
        let mut claimed_any = false;
        for snapshot in store.list_records()? {
            if shared.should_stop() {
                return Ok(());
            }
            if snapshot.status != "queued" {
                continue;
            }
            let cancel_token = next_cancel_token(&execution_owner, &snapshot.task_id)?;
            let Some(claim) = store.try_claim_queued(
                &snapshot.task_id,
                &execution_owner,
                &cancel_token,
                worker_now_ms()?,
            )?
            else {
                continue;
            };
            claimed_any = true;
            execute_task_claim(
                &mut store,
                registry.as_ref(),
                artifacts.as_ref(),
                &shared,
                claim,
            )?;
        }
        if !claimed_any {
            shared.wait_for_work();
        }
    }
}

fn execute_task_claim(
    store: &mut FileSystemTaskStateStore,
    registry: &TaskHandlerRegistry,
    artifacts: &dyn TaskArtifactResolver,
    shared: &TaskWorkerShared,
    claim: TaskExecutionClaim,
) -> Result<(), EvaError> {
    let task_id = RequestId::parse(&claim.snapshot().task_id)?;
    let cancel_token = claim.fence().cancel_token().to_owned();
    let cancellation = TaskCancellationView::default();
    shared.register_attempt(task_id.as_str(), &cancel_token, cancellation.clone())?;

    let execution = (|| {
        let mut latest = store.read(Some(task_id.as_str()))?;
        if !latest.cancel_requested && shared.should_stop() {
            cancellation.request();
            latest = store.request_cancellation(
                task_id.as_str(),
                "daemon shutdown preceded handler dispatch",
            )?;
        }
        let cancel_precedes_dispatch =
            latest.cancel_requested || matches!(latest.status.as_str(), "cancelling" | "cancelled");
        if cancel_precedes_dispatch {
            cancellation.request();
        }
        let observed_before_dispatch = worker_now_ms()?;
        let outcome = if cancel_precedes_dispatch {
            TaskAttemptOutcome::Failed {
                error_kind: ErrorKind::Conflict.as_str().to_owned(),
                error_message: "task cancellation preceded handler dispatch".to_owned(),
            }
        } else if claim.snapshot().deadline_expired(observed_before_dispatch) {
            TaskAttemptOutcome::TimedOut {
                observed_at_ms: observed_before_dispatch,
            }
        } else {
            dispatch_claimed_attempt(
                registry,
                artifacts,
                &task_id,
                claim.snapshot(),
                &cancellation,
            )?
        };
        store.finish_execution(claim.fence(), &outcome)?;
        Ok(())
    })();

    shared.unregister_attempt(task_id.as_str(), &cancel_token);
    execution
}

fn dispatch_claimed_attempt(
    registry: &TaskHandlerRegistry,
    artifacts: &dyn TaskArtifactResolver,
    task_id: &RequestId,
    snapshot: &eva_storage::TaskStateSnapshot,
    cancellation: &TaskCancellationView,
) -> Result<TaskAttemptOutcome, EvaError> {
    let envelope = snapshot
        .envelope
        .clone()
        .ok_or_else(|| EvaError::internal("claimed task is missing its envelope"))
        .and_then(TaskEnvelope::try_from);
    let dispatched = envelope.and_then(|envelope| {
        catch_unwind(AssertUnwindSafe(|| {
            registry.dispatch_attempt(
                task_id,
                &envelope,
                artifacts,
                snapshot.attempts,
                snapshot.deadline_at_ms,
                cancellation,
            )
        }))
        .unwrap_or_else(|_| Err(EvaError::internal(TASK_HANDLER_PANIC_MESSAGE)))
    });
    let observed_at_ms = worker_now_ms()?;
    if snapshot.deadline_expired(observed_at_ms) {
        return Ok(TaskAttemptOutcome::TimedOut { observed_at_ms });
    }
    Ok(match dispatched {
        Ok(result) => TaskAttemptOutcome::Completed {
            result_digest: result.digest().to_owned(),
            result_size_bytes: result.size_bytes(),
        },
        Err(error) if error.kind() == ErrorKind::Timeout => {
            TaskAttemptOutcome::TimedOut { observed_at_ms }
        }
        Err(error) => TaskAttemptOutcome::Failed {
            error_kind: error.kind().as_str().to_owned(),
            error_message: error.message().to_owned(),
        },
    })
}

fn next_cancel_token(execution_owner: &str, task_id: &str) -> Result<String, EvaError> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let material = format!(
        "{execution_owner}:{task_id}:{}:{sequence}",
        worker_now_ms()?
    );
    Ok(format!(
        "task-cancel:{}",
        sha256_digest(material.as_bytes())
    ))
}

fn worker_now_ms() -> Result<u128, EvaError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|error| {
            EvaError::internal("system clock is before the Unix epoch")
                .with_context("clock_error", error.to_string())
        })
}

impl fmt::Debug for TaskHandlerRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskHandlerRegistry")
            .field("registered_kinds", &self.registered_kinds())
            .finish()
    }
}

fn runtime_echo_task_handler(
    invocation: &TaskHandlerInvocation<'_>,
) -> Result<TaskHandlerResult, EvaError> {
    Ok(TaskHandlerResult::new(invocation.payload()))
}

fn resolve_task_payload(
    envelope: &TaskEnvelope,
    artifacts: &dyn TaskArtifactResolver,
) -> Result<(Vec<u8>, String), EvaError> {
    match envelope.input() {
        TaskInput::Inline { bytes, digest } => {
            let actual_digest = sha256_digest(bytes);
            if actual_digest != *digest {
                return Err(EvaError::conflict("task inline input digest mismatch")
                    .with_context("task_kind", envelope.kind().as_str())
                    .with_context("expected_digest", digest)
                    .with_context("actual_digest", actual_digest));
            }
            Ok((bytes.clone(), digest.clone()))
        }
        TaskInput::Artifact(reference) => {
            let record = artifacts.resolve_task_artifact(reference)?.ok_or_else(|| {
                EvaError::not_found("task input artifact is missing")
                    .with_context("task_kind", envelope.kind().as_str())
                    .with_context("artifact_key", reference.key())
            })?;
            if record.key != reference.key() {
                return Err(EvaError::conflict("task input artifact key mismatch")
                    .with_context("task_kind", envelope.kind().as_str())
                    .with_context("expected_artifact_key", reference.key())
                    .with_context("actual_artifact_key", record.key));
            }
            if record.size_bytes != record.bytes.len() {
                return Err(EvaError::conflict("task input artifact size mismatch")
                    .with_context("task_kind", envelope.kind().as_str())
                    .with_context("artifact_key", reference.key())
                    .with_context("expected_size", record.size_bytes.to_string())
                    .with_context("actual_size", record.bytes.len().to_string()));
            }
            let actual_digest = sha256_digest(&record.bytes);
            if record.digest != actual_digest {
                return Err(
                    EvaError::conflict("task input artifact record digest mismatch")
                        .with_context("task_kind", envelope.kind().as_str())
                        .with_context("artifact_key", reference.key())
                        .with_context("record_digest", record.digest)
                        .with_context("actual_digest", actual_digest),
                );
            }
            if actual_digest != reference.digest() {
                return Err(EvaError::conflict(
                    "task input artifact digest does not match envelope",
                )
                .with_context("task_kind", envelope.kind().as_str())
                .with_context("artifact_key", reference.key())
                .with_context("expected_digest", reference.digest())
                .with_context("actual_digest", actual_digest));
            }
            Ok((record.bytes, actual_digest))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IdempotencyKey, TaskAttemptPolicy};
    use eva_core::{AgentId, ErrorKind};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn task_id() -> RequestId {
        RequestId::parse("req-task-handler").unwrap()
    }

    fn envelope(kind: &str, input: TaskInput) -> TaskEnvelope {
        TaskEnvelope::new(
            TaskKind::parse(kind).unwrap(),
            AgentId::parse("root-agent").unwrap(),
            input,
            IdempotencyKey::parse("idem-task-handler").unwrap(),
            TaskAttemptPolicy::new(1, 0, None).unwrap(),
        )
        .unwrap()
    }

    fn context_value<'a>(error: &'a EvaError, key: &str) -> Option<&'a str> {
        error
            .context()
            .entries()
            .iter()
            .find_map(|(name, value)| (name == key).then_some(value.as_str()))
    }

    #[test]
    fn registered_handler_receives_exact_inline_payload_and_returns_bound_result() {
        let payload = vec![0, 0xff, b'\n', b'=', 0x80];
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_by_handler = Arc::clone(&seen);
        let mut registry = TaskHandlerRegistry::new();
        registry
            .register(
                TaskKind::parse("vendor.binary").unwrap(),
                move |invocation: &TaskHandlerInvocation<'_>| {
                    *seen_by_handler.lock().unwrap() = invocation.payload().to_vec();
                    assert_eq!(invocation.task_id().as_str(), "req-task-handler");
                    assert_eq!(invocation.envelope().agent_id().as_str(), "root-agent");
                    assert_eq!(
                        invocation.payload_digest(),
                        sha256_digest(invocation.payload())
                    );
                    Ok(TaskHandlerResult::new(
                        [b"result:".as_slice(), invocation.payload()].concat(),
                    ))
                },
            )
            .unwrap();
        let envelope = envelope("vendor.binary", TaskInput::inline(payload.clone()).unwrap());
        let reopened = TaskEnvelope::try_from(envelope.to_snapshot()).unwrap();

        let result = registry
            .dispatch(&task_id(), &reopened, &InMemoryArtifactStore::new())
            .unwrap();

        assert_eq!(*seen.lock().unwrap(), payload);
        assert_eq!(
            result.bytes(),
            [b"result:".as_slice(), payload.as_slice()].concat()
        );
        assert_eq!(result.digest(), sha256_digest(result.bytes()));
    }

    #[test]
    fn unknown_kind_fails_stably_before_artifact_access() {
        let resolver = CountingResolver::default();
        let envelope = envelope(
            "vendor.unknown",
            TaskInput::artifact(
                TaskArtifactRef::new("tasks/input", format!("sha256:{}", "0".repeat(64))).unwrap(),
            ),
        );

        let error = TaskHandlerRegistry::new()
            .dispatch(&task_id(), &envelope, &resolver)
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert_eq!(error.message(), TASK_HANDLER_NOT_REGISTERED_MESSAGE);
        assert!(!error.is_retryable());
        assert_eq!(context_value(&error, "task_kind"), Some("vendor.unknown"));
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn duplicate_registration_fails_without_replacing_first_handler() {
        let kind = TaskKind::parse("vendor.duplicate").unwrap();
        let mut registry = TaskHandlerRegistry::new();
        registry
            .register(kind.clone(), |_: &TaskHandlerInvocation<'_>| {
                Ok(TaskHandlerResult::new(b"first".as_slice()))
            })
            .unwrap();

        let error = registry
            .register(kind, |_: &TaskHandlerInvocation<'_>| {
                Ok(TaskHandlerResult::new(b"second".as_slice()))
            })
            .unwrap_err();
        let result = registry
            .dispatch(
                &task_id(),
                &envelope(
                    "vendor.duplicate",
                    TaskInput::inline(b"payload".as_slice()).unwrap(),
                ),
                &InMemoryArtifactStore::new(),
            )
            .unwrap();

        assert_eq!(error.kind(), ErrorKind::Conflict);
        assert_eq!(error.message(), "task handler kind is already registered");
        assert_eq!(result.bytes(), b"first");
    }

    #[test]
    fn artifact_payload_is_loaded_and_reverified_before_handler() {
        let payload = vec![0xde, 0xad, 0, 0xbe, 0xef];
        let mut artifacts = InMemoryArtifactStore::new();
        let record = artifacts
            .put_bytes("tasks/artifact-input", payload.clone())
            .unwrap();
        let task = envelope(
            "runtime.echo",
            TaskInput::artifact(
                TaskArtifactRef::new(record.key.clone(), record.digest.clone()).unwrap(),
            ),
        );

        let result = TaskHandlerRegistry::with_runtime_defaults()
            .unwrap()
            .dispatch(&task_id(), &task, &artifacts)
            .unwrap();

        assert_eq!(result.bytes(), payload);
        assert_eq!(result.digest(), record.digest);
    }

    #[test]
    fn artifact_digest_mismatch_fails_without_calling_handler() {
        let mut artifacts = InMemoryArtifactStore::new();
        artifacts
            .put_bytes("tasks/tampered", b"actual".as_slice())
            .unwrap();
        let task = envelope(
            "vendor.checked",
            TaskInput::artifact(
                TaskArtifactRef::new("tasks/tampered", format!("sha256:{}", "0".repeat(64)))
                    .unwrap(),
            ),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = Arc::clone(&calls);
        let mut registry = TaskHandlerRegistry::new();
        registry
            .register(
                TaskKind::parse("vendor.checked").unwrap(),
                move |_: &TaskHandlerInvocation<'_>| {
                    handler_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(TaskHandlerResult::new(Vec::<u8>::new()))
                },
            )
            .unwrap();

        let error = registry
            .dispatch(&task_id(), &task, &artifacts)
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Conflict);
        assert_eq!(
            error.message(),
            "task input artifact digest does not match envelope"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn missing_artifact_fails_without_calling_handler() {
        let task = envelope(
            "vendor.missing-input",
            TaskInput::artifact(
                TaskArtifactRef::new("tasks/missing", format!("sha256:{}", "0".repeat(64)))
                    .unwrap(),
            ),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = Arc::clone(&calls);
        let mut registry = TaskHandlerRegistry::new();
        registry
            .register(
                TaskKind::parse("vendor.missing-input").unwrap(),
                move |_: &TaskHandlerInvocation<'_>| {
                    handler_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(TaskHandlerResult::new(Vec::<u8>::new()))
                },
            )
            .unwrap();

        let error = registry
            .dispatch(&task_id(), &task, &InMemoryArtifactStore::new())
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert_eq!(error.message(), "task input artifact is missing");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn resolver_record_corruption_is_rejected_before_handler() {
        let bytes = b"artifact".to_vec();
        let digest = sha256_digest(&bytes);
        let task = envelope(
            "vendor.corrupt-record",
            TaskInput::artifact(TaskArtifactRef::new("tasks/corrupt", digest.clone()).unwrap()),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = Arc::clone(&calls);
        let mut registry = TaskHandlerRegistry::new();
        registry
            .register(
                TaskKind::parse("vendor.corrupt-record").unwrap(),
                move |_: &TaskHandlerInvocation<'_>| {
                    handler_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(TaskHandlerResult::new(Vec::<u8>::new()))
                },
            )
            .unwrap();

        let corrupt_records = [
            (
                ArtifactRecord {
                    key: "tasks/other".to_owned(),
                    bytes: bytes.clone(),
                    digest: digest.clone(),
                    size_bytes: bytes.len(),
                    content_type: "application/octet-stream".to_owned(),
                    retention_policy: "retain".to_owned(),
                    retain_until_ms: None,
                },
                "task input artifact key mismatch",
            ),
            (
                ArtifactRecord {
                    key: "tasks/corrupt".to_owned(),
                    bytes: bytes.clone(),
                    digest: digest.clone(),
                    size_bytes: bytes.len() + 1,
                    content_type: "application/octet-stream".to_owned(),
                    retention_policy: "retain".to_owned(),
                    retain_until_ms: None,
                },
                "task input artifact size mismatch",
            ),
            (
                ArtifactRecord {
                    key: "tasks/corrupt".to_owned(),
                    bytes,
                    digest: format!("sha256:{}", "0".repeat(64)),
                    size_bytes: 8,
                    content_type: "application/octet-stream".to_owned(),
                    retention_policy: "retain".to_owned(),
                    retain_until_ms: None,
                },
                "task input artifact record digest mismatch",
            ),
        ];

        for (record, expected_message) in corrupt_records {
            let error = registry
                .dispatch(&task_id(), &task, &FixedResolver(Some(record)))
                .unwrap_err();

            assert_eq!(error.kind(), ErrorKind::Conflict);
            assert_eq!(error.message(), expected_message);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn handler_error_preserves_kind_and_retryability_with_safe_identity_context() {
        let mut registry = TaskHandlerRegistry::new();
        registry
            .register(
                TaskKind::parse("vendor.retryable").unwrap(),
                |_: &TaskHandlerInvocation<'_>| -> Result<TaskHandlerResult, EvaError> {
                    Err(EvaError::unavailable("handler is temporarily unavailable"))
                },
            )
            .unwrap();
        let task = envelope(
            "vendor.retryable",
            TaskInput::inline(b"secret".as_slice()).unwrap(),
        );

        let error = registry
            .dispatch(&task_id(), &task, &InMemoryArtifactStore::new())
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert!(error.is_retryable());
        assert_eq!(context_value(&error, "task_kind"), Some("vendor.retryable"));
        assert_eq!(context_value(&error, "agent_id"), Some("root-agent"));
        assert!(!format!("{error:?}").contains("secret"));
    }

    #[test]
    fn filesystem_resolver_enforces_limit_and_dispatches_verified_bytes() {
        let root = test_root("filesystem-resolver");
        let payload = b"filesystem-task-input";
        let mut writer = FileSystemArtifactStore::new(root.path());
        let record = writer
            .put_bytes("tasks/filesystem", payload.as_slice())
            .unwrap();
        let task = envelope(
            "runtime.echo",
            TaskInput::artifact(TaskArtifactRef::new(record.key, record.digest).unwrap()),
        );
        let registry = TaskHandlerRegistry::with_runtime_defaults().unwrap();

        let error = registry
            .dispatch(
                &task_id(),
                &task,
                &FileSystemTaskArtifactResolver::new(root.path(), payload.len() - 1),
            )
            .unwrap_err();
        let resolver = FileSystemTaskArtifactResolver::new(root.path(), payload.len());
        let result = registry.dispatch(&task_id(), &task, &resolver).unwrap();

        assert_eq!(error.kind(), ErrorKind::Conflict);
        assert_eq!(error.message(), "artifact exceeds configured read limit");
        assert_eq!(resolver.root(), root.path());
        assert_eq!(resolver.max_size_bytes(), payload.len());
        assert_eq!(result.bytes(), payload);
    }

    #[test]
    fn default_registry_executes_echo_but_not_legacy_submit() {
        let registry = TaskHandlerRegistry::with_runtime_defaults().unwrap();
        assert_eq!(registry.registered_kinds(), vec!["runtime.echo"]);
        assert!(registry.contains(&TaskKind::parse("runtime.echo").unwrap()));

        let legacy = envelope(
            "legacy.submit",
            TaskInput::inline(Vec::<u8>::new()).unwrap(),
        );
        let error = registry
            .dispatch(&task_id(), &legacy, &InMemoryArtifactStore::new())
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert_eq!(error.message(), TASK_HANDLER_NOT_REGISTERED_MESSAGE);
    }

    #[test]
    fn invocation_and_result_debug_output_redacts_bytes() {
        let payload = b"private-task-input";
        let task = envelope(
            "runtime.echo",
            TaskInput::inline(payload.as_slice()).unwrap(),
        );
        let digest = sha256_digest(payload);
        let id = task_id();
        let cancellation = TaskCancellationView::default();
        let invocation = TaskHandlerInvocation {
            task_id: &id,
            envelope: &task,
            payload,
            payload_digest: &digest,
            attempt: 1,
            deadline_at_ms: Some(123),
            cancellation: &cancellation,
        };
        let result = TaskHandlerResult::new(payload.as_slice());

        let invocation_debug = format!("{invocation:?}");
        let result_debug = format!("{result:?}");
        assert!(invocation_debug.contains("<redacted>"));
        assert!(result_debug.contains("<redacted>"));
        assert!(!invocation_debug.contains("private-task-input"));
        assert!(!result_debug.contains("private-task-input"));
    }

    #[test]
    fn two_workers_claim_and_execute_one_task_exactly_once() {
        let root = test_root("two-workers-one-claim");
        let backend = eva_storage::FileSystemDurableBackend::open(
            eva_storage::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let mut store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let task = envelope(
            "vendor.once",
            TaskInput::inline(b"execute-once".as_slice()).unwrap(),
        );
        store
            .create(
                &eva_storage::TaskStateSnapshot::queued_with_envelope(
                    "req-worker-once",
                    task.to_snapshot(),
                )
                .unwrap(),
            )
            .unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_by_handler = Arc::clone(&calls);
        let mut registry = TaskHandlerRegistry::new();
        registry
            .register(
                TaskKind::parse("vendor.once").unwrap(),
                move |invocation: &TaskHandlerInvocation<'_>| {
                    assert_eq!(invocation.attempt(), 1);
                    assert!(!invocation.cancellation().is_requested());
                    calls_by_handler.fetch_add(1, Ordering::SeqCst);
                    Ok(TaskHandlerResult::new(invocation.payload()))
                },
            )
            .unwrap();
        let registry = Arc::new(registry);
        let artifacts: Arc<dyn TaskArtifactResolver> = Arc::new(InMemoryArtifactStore::new());
        let mut first = TaskWorkerRuntime::start(
            store.clone(),
            Arc::clone(&registry),
            Arc::clone(&artifacts),
            "daemon:g1:worker-1",
        )
        .unwrap();
        let mut second =
            TaskWorkerRuntime::start(store.clone(), registry, artifacts, "daemon:g1:worker-2")
                .unwrap();
        first.notify_new_work();
        second.notify_new_work();

        let completed = wait_for_task_status(&store, "req-worker-once", "completed");
        first.stop_and_join().unwrap();
        second.stop_and_join().unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(completed.attempts, 1);
        assert_eq!(completed.owner_generation.0, 1);
        assert!(completed.execution_owner.is_some());
        assert!(completed.cancel_token.is_some());
        assert_eq!(completed.result_size_bytes, Some(b"execute-once".len()));
        assert_eq!(
            completed.result_digest.as_deref(),
            Some(sha256_digest(b"execute-once").as_str())
        );
    }

    #[test]
    fn paused_worker_does_not_claim_until_readiness_gate_is_activated() {
        let root = test_root("paused-ready-gate");
        let mut store = FileSystemTaskStateStore::new(root.path());
        let task = envelope(
            "runtime.echo",
            TaskInput::inline(b"ready-gated".as_slice()).unwrap(),
        );
        store
            .create(
                &eva_storage::TaskStateSnapshot::queued_with_envelope(
                    "req-worker-ready-gate",
                    task.to_snapshot(),
                )
                .unwrap(),
            )
            .unwrap();
        let mut worker = TaskWorkerRuntime::start_paused(
            store.clone(),
            Arc::new(TaskHandlerRegistry::with_runtime_defaults().unwrap()),
            Arc::new(InMemoryArtifactStore::new()),
            "daemon:g1:worker-ready-gate",
        )
        .unwrap();

        thread::sleep(Duration::from_millis(
            DEFAULT_TASK_WORKER_POLL_INTERVAL_MS * 3,
        ));
        let queued = store.read(Some("req-worker-ready-gate")).unwrap();
        assert_eq!(queued.status, "queued");
        assert_eq!(queued.attempts, 0);

        worker.activate();
        let completed = wait_for_task_status(&store, "req-worker-ready-gate", "completed");
        worker.stop_and_join().unwrap();
        assert_eq!(completed.attempts, 1);
    }

    #[test]
    fn worker_maps_unknown_error_panic_and_deadline_to_terminal_states() {
        let root = test_root("worker-terminal-mapping");
        let mut store = FileSystemTaskStateStore::new(root.path());
        let tasks = [
            ("req-worker-unknown", "vendor.unknown", None),
            ("req-worker-error", "vendor.error", None),
            ("req-worker-panic", "vendor.panic", None),
            ("req-worker-timeout", "vendor.timeout", Some(1)),
        ];
        for (task_id, kind, timeout_ms) in tasks {
            let task = TaskEnvelope::new(
                TaskKind::parse(kind).unwrap(),
                AgentId::parse("root-agent").unwrap(),
                TaskInput::inline(task_id.as_bytes()).unwrap(),
                IdempotencyKey::parse(task_id).unwrap(),
                TaskAttemptPolicy::new(1, 0, timeout_ms).unwrap(),
            )
            .unwrap();
            store
                .create(
                    &eva_storage::TaskStateSnapshot::queued_with_envelope(
                        task_id,
                        task.to_snapshot(),
                    )
                    .unwrap(),
                )
                .unwrap();
        }

        let mut registry = TaskHandlerRegistry::new();
        registry
            .register(
                TaskKind::parse("vendor.error").unwrap(),
                |_: &TaskHandlerInvocation<'_>| Err(EvaError::unavailable("handler unavailable")),
            )
            .unwrap();
        registry
            .register(
                TaskKind::parse("vendor.panic").unwrap(),
                |_: &TaskHandlerInvocation<'_>| -> Result<TaskHandlerResult, EvaError> {
                    panic!("private panic payload")
                },
            )
            .unwrap();
        registry
            .register(
                TaskKind::parse("vendor.timeout").unwrap(),
                |_: &TaskHandlerInvocation<'_>| {
                    thread::sleep(Duration::from_millis(10));
                    Ok(TaskHandlerResult::new(b"late".as_slice()))
                },
            )
            .unwrap();
        let mut worker = TaskWorkerRuntime::start(
            store.clone(),
            Arc::new(registry),
            Arc::new(InMemoryArtifactStore::new()),
            "daemon:g1:worker-terminal",
        )
        .unwrap();
        worker.notify_new_work();

        let unknown = wait_for_task_status(&store, "req-worker-unknown", "failed");
        let failed = wait_for_task_status(&store, "req-worker-error", "failed");
        let panicked = wait_for_task_status(&store, "req-worker-panic", "failed");
        let timed_out = wait_for_task_status(&store, "req-worker-timeout", "timed_out");
        worker.check_health().unwrap();
        worker.stop_and_join().unwrap();

        assert_eq!(unknown.error_kind.as_deref(), Some("not_found"));
        assert_eq!(
            unknown.error_message.as_deref(),
            Some(TASK_HANDLER_NOT_REGISTERED_MESSAGE)
        );
        assert_eq!(failed.error_kind.as_deref(), Some("unavailable"));
        assert_eq!(failed.error_message.as_deref(), Some("handler unavailable"));
        assert_eq!(panicked.error_kind.as_deref(), Some("internal"));
        assert_eq!(
            panicked.error_message.as_deref(),
            Some(TASK_HANDLER_PANIC_MESSAGE)
        );
        assert!(!format!("{panicked:?}").contains("private panic payload"));
        assert_eq!(timed_out.error_kind.as_deref(), Some("timeout"));
        assert_eq!(timed_out.result_digest, None);
    }

    #[test]
    fn durable_running_cancel_reaches_matching_handler_and_wins_terminal_race() {
        let root = test_root("worker-running-cancel");
        let mut store = FileSystemTaskStateStore::new(root.path());
        let task = envelope(
            "vendor.cancel-aware",
            TaskInput::inline(b"cancel-me".as_slice()).unwrap(),
        );
        store
            .create(
                &eva_storage::TaskStateSnapshot::queued_with_envelope(
                    "req-worker-cancel",
                    task.to_snapshot(),
                )
                .unwrap(),
            )
            .unwrap();

        let handler_started = Arc::new(AtomicBool::new(false));
        let started_by_handler = Arc::clone(&handler_started);
        let mut registry = TaskHandlerRegistry::new();
        registry
            .register(
                TaskKind::parse("vendor.cancel-aware").unwrap(),
                move |invocation: &TaskHandlerInvocation<'_>| {
                    started_by_handler.store(true, Ordering::Release);
                    let started_at = Instant::now();
                    while !invocation.cancellation().is_requested() {
                        assert!(
                            started_at.elapsed() < Duration::from_secs(2),
                            "handler did not receive cancellation"
                        );
                        thread::yield_now();
                    }
                    Ok(TaskHandlerResult::new(b"late-success".as_slice()))
                },
            )
            .unwrap();
        let mut worker = TaskWorkerRuntime::start(
            store.clone(),
            Arc::new(registry),
            Arc::new(InMemoryArtifactStore::new()),
            "daemon:g1:worker-cancel",
        )
        .unwrap();
        worker.notify_new_work();
        wait_until(Duration::from_secs(2), || {
            handler_started.load(Ordering::Acquire)
        });

        let cancelling = store
            .request_cancellation("req-worker-cancel", "operator stop")
            .unwrap();
        assert!(worker.signal_cancellation("req-worker-cancel", cancelling.cancel_token.as_deref()));
        let cancelled = wait_for_task_status(&store, "req-worker-cancel", "cancelled");
        worker.stop_and_join().unwrap();

        assert!(cancelled.cancel_requested);
        assert!(cancelled.cancel_accepted);
        assert_eq!(cancelled.result_digest, None);
        assert_eq!(cancelled.error_kind, None);
    }

    #[test]
    fn shutdown_closes_claim_gate_before_active_handler_returns() {
        let root = test_root("worker-shutdown-claim-gate");
        let mut store = FileSystemTaskStateStore::new(root.path());
        for task_id in ["req-worker-shutdown-1", "req-worker-shutdown-2"] {
            let task = TaskEnvelope::new(
                TaskKind::parse("vendor.shutdown-aware").unwrap(),
                AgentId::parse("root-agent").unwrap(),
                TaskInput::inline(task_id.as_bytes()).unwrap(),
                IdempotencyKey::parse(task_id).unwrap(),
                TaskAttemptPolicy::new(1, 0, None).unwrap(),
            )
            .unwrap();
            store
                .create(
                    &eva_storage::TaskStateSnapshot::queued_with_envelope(
                        task_id,
                        task.to_snapshot(),
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_by_handler = Arc::clone(&calls);
        let mut registry = TaskHandlerRegistry::new();
        registry
            .register(
                TaskKind::parse("vendor.shutdown-aware").unwrap(),
                move |invocation: &TaskHandlerInvocation<'_>| {
                    calls_by_handler.fetch_add(1, Ordering::SeqCst);
                    let started_at = Instant::now();
                    while !invocation.cancellation().is_requested() {
                        assert!(started_at.elapsed() < Duration::from_secs(2));
                        thread::yield_now();
                    }
                    Ok(TaskHandlerResult::new(b"stopped".as_slice()))
                },
            )
            .unwrap();
        let mut worker = TaskWorkerRuntime::start(
            store.clone(),
            Arc::new(registry),
            Arc::new(InMemoryArtifactStore::new()),
            "daemon:g1:worker-shutdown",
        )
        .unwrap();
        wait_until(Duration::from_secs(2), || calls.load(Ordering::SeqCst) == 1);

        worker.begin_shutdown();
        worker.stop_and_join().unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            store.read(Some("req-worker-shutdown-1")).unwrap().status,
            "completed"
        );
        assert_eq!(
            store.read(Some("req-worker-shutdown-2")).unwrap().status,
            "queued"
        );
    }

    #[derive(Default)]
    struct CountingResolver {
        calls: AtomicUsize,
    }

    impl TaskArtifactResolver for CountingResolver {
        fn resolve_task_artifact(
            &self,
            _reference: &TaskArtifactRef,
        ) -> Result<Option<ArtifactRecord>, EvaError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }
    }

    struct FixedResolver(Option<ArtifactRecord>);

    impl TaskArtifactResolver for FixedResolver {
        fn resolve_task_artifact(
            &self,
            _reference: &TaskArtifactRef,
        ) -> Result<Option<ArtifactRecord>, EvaError> {
            Ok(self.0.clone())
        }
    }

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn test_root(name: &str) -> TestRoot {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let root = std::env::temp_dir().join(format!(
            "eva-task-handler-{name}-{}-{timestamp}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        TestRoot(root)
    }

    fn wait_for_task_status(
        store: &FileSystemTaskStateStore,
        task_id: &str,
        expected_status: &str,
    ) -> eva_storage::TaskStateSnapshot {
        let started_at = Instant::now();
        loop {
            let snapshot = store.read(Some(task_id)).unwrap();
            if snapshot.status == expected_status {
                return snapshot;
            }
            assert!(
                started_at.elapsed() < Duration::from_secs(10),
                "task {task_id} remained in status {} while waiting for {expected_status}",
                snapshot.status
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_until(timeout: Duration, condition: impl Fn() -> bool) {
        let started_at = Instant::now();
        while !condition() {
            assert!(started_at.elapsed() < timeout, "condition timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }
}
