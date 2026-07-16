//! Durable task handler lookup and payload integrity boundary.

use crate::{
    IdempotencyKey, TaskArtifactRef, TaskAttemptPolicy, TaskEnvelope, TaskInput, TaskKind,
};
use eva_core::{ErrorKind, EvaError, Event, EventId, EventPayload, EventTarget, RequestId, Topic};
use eva_eventbus::{
    DeadLetterRecord, DurableEventBus, EventBus, RedrivePolicy, ReplayHandlerBinding,
};
use eva_storage::{
    artifact_store::sha256_digest, ArtifactRecord, ArtifactStore, EffectLedgerIntent,
    EffectLedgerRecord, EffectLedgerState, EffectOperationIdentity, EffectPrepareOutcome,
    FileSystemArtifactStore, FileSystemEffectLedger, FileSystemTaskStateStore,
    InMemoryArtifactStore, TaskAttemptFence, TaskAttemptOutcome, TaskExecutionClaim,
    TaskStateDeadLetterSnapshot, TaskStateSnapshot, TaskStateStore,
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
/// Persisted heartbeat cadence for an active task attempt.
pub const DEFAULT_TASK_HEARTBEAT_INTERVAL_MS: u64 = 1_000;

/// Timing knobs for the worker scan and per-attempt heartbeat loops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TaskWorkerTiming {
    /// Idle scan interval.
    poll_interval: Duration,
    /// Active-attempt heartbeat interval.
    heartbeat_interval: Duration,
}

impl Default for TaskWorkerTiming {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(DEFAULT_TASK_WORKER_POLL_INTERVAL_MS),
            heartbeat_interval: Duration::from_millis(DEFAULT_TASK_HEARTBEAT_INTERVAL_MS),
        }
    }
}

impl TaskWorkerTiming {
    /// Build timing values while rejecting a busy-loop configuration.
    #[cfg(test)]
    fn new(poll_interval: Duration, heartbeat_interval: Duration) -> Result<Self, EvaError> {
        Self {
            poll_interval,
            heartbeat_interval,
        }
        .validate()
    }

    fn validate(self) -> Result<Self, EvaError> {
        if self.poll_interval.is_zero() || self.heartbeat_interval.is_zero() {
            return Err(EvaError::invalid_argument(
                "task worker timing intervals must be greater than zero",
            ));
        }
        Ok(self)
    }
}

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

/// Durable state of one replay delivery owned by the daemon task worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedReplayDeliveryStatus {
    /// The deterministic delivery task is queued, running, or cancelling.
    Pending {
        /// Durable task identity derived from replay event and delivery index.
        task_id: String,
        /// Current task lifecycle status.
        status: String,
    },
    /// The fenced delivery task completed and will never be invoked again.
    Succeeded {
        /// Durable task identity derived from replay event and delivery index.
        task_id: String,
        /// Canonical digest persisted by the worker finish boundary.
        result_digest: String,
        /// Result size persisted beside the digest.
        result_size_bytes: usize,
    },
    /// The latest attempt failed; a retry may already have been requeued.
    Failed {
        /// Durable task identity derived from replay event and delivery index.
        task_id: String,
        /// Structured failure reconstructed from the terminal task record.
        error: EvaError,
        /// Whether this reconciliation durably returned the delivery to queued.
        retry_scheduled: bool,
    },
}

/// Reconciles replay deliveries through the daemon's durable task claim loop.
pub trait OwnedReplayHandler: Send + Sync {
    /// Creates or inspects one deterministic delivery task without running the handler inline.
    fn reconcile_replay_delivery(
        &self,
        binding: &ReplayHandlerBinding,
        event: &Event,
        delivery_index: usize,
        retry_backoff_ms: u64,
        observed_at_ms: u64,
    ) -> Result<OwnedReplayDeliveryStatus, EvaError>;
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
    handlers: BTreeMap<TaskKind, TaskHandlerRegistration>,
}

struct TaskHandlerRegistration {
    handler: Arc<dyn TaskHandler>,
    effect_scope: Option<String>,
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
        self.register_with_effect_scope(kind, None, handler)
    }

    /// Registers a non-idempotent handler under one stable effect contract/slot identity.
    ///
    /// Such a handler can run only through a worker configured with a durable effect ledger.
    pub fn register_non_idempotent<H>(
        &mut self,
        kind: TaskKind,
        effect_scope: impl Into<String>,
        handler: H,
    ) -> Result<(), EvaError>
    where
        H: TaskHandler + 'static,
    {
        let effect_scope = effect_scope.into();
        RequestId::parse(&effect_scope).map_err(|error| {
            EvaError::invalid_argument("task handler effect scope is invalid")
                .with_context("cause", error.message())
        })?;
        self.register_with_effect_scope(kind, Some(effect_scope), handler)
    }

    fn register_with_effect_scope<H>(
        &mut self,
        kind: TaskKind,
        effect_scope: Option<String>,
        handler: H,
    ) -> Result<(), EvaError>
    where
        H: TaskHandler + 'static,
    {
        if self.handlers.contains_key(&kind) {
            return Err(
                EvaError::conflict("task handler kind is already registered")
                    .with_context("task_kind", kind.as_str()),
            );
        }
        self.handlers.insert(
            kind,
            TaskHandlerRegistration {
                handler: Arc::new(handler),
                effect_scope,
            },
        );
        Ok(())
    }

    /// Returns whether this exact validated kind has a handler.
    pub fn contains(&self, kind: &TaskKind) -> bool {
        self.handlers.contains_key(kind)
    }

    /// Returns the durable effect contract for a non-idempotent handler.
    ///
    /// `None` means either the handler is pure/idempotent or the kind is not registered; callers
    /// that need to distinguish those cases must check `contains` first.
    pub fn non_idempotent_effect_scope(&self, kind: &TaskKind) -> Option<&str> {
        self.handlers
            .get(kind)
            .and_then(|registration| registration.effect_scope.as_deref())
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
        let registration = self.handlers.get(envelope.kind()).ok_or_else(|| {
            EvaError::not_found(TASK_HANDLER_NOT_REGISTERED_MESSAGE)
                .with_context("task_id", task_id.as_str())
                .with_context("task_kind", envelope.kind().as_str())
                .with_context("agent_id", envelope.agent_id().as_str())
        })?;
        if registration.effect_scope.is_some() {
            return Err(EvaError::unavailable(
                "non-idempotent task handler requires a durable effect ledger",
            )
            .with_retryable(false)
            .with_context("task_id", task_id.as_str())
            .with_context("task_kind", envelope.kind().as_str())
            .with_context("agent_id", envelope.agent_id().as_str()));
        }
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
        registration.handler.handle(&invocation).map_err(|error| {
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
    poll_interval: Duration,
    wait_lock: Mutex<()>,
    wake: Condvar,
    active: Mutex<BTreeMap<String, ActiveTaskAttempt>>,
    fatal_error: Mutex<Option<EvaError>>,
}

impl TaskWorkerShared {
    fn new(timing: TaskWorkerTiming) -> Self {
        Self {
            stop_requested: AtomicBool::new(false),
            claims_enabled: AtomicBool::new(false),
            poll_interval: timing.poll_interval,
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
            let _ = self.wake.wait_timeout(guard, self.poll_interval);
        }
    }
}

/// Daemon-owned durable task claim and execution thread.
pub struct TaskWorkerRuntime {
    shared: Arc<TaskWorkerShared>,
    store: FileSystemTaskStateStore,
    execution_owner: String,
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
        let worker = Self::start_paused_with_timing(
            store,
            registry,
            artifacts,
            execution_owner,
            TaskWorkerTiming::default(),
        )?;
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
        Self::start_paused_with_timing(
            store,
            registry,
            artifacts,
            execution_owner,
            TaskWorkerTiming::default(),
        )
    }

    /// Creates a paused worker that durably binds failed production tasks for replay.
    pub fn start_paused_with_failure_bus(
        store: FileSystemTaskStateStore,
        registry: Arc<TaskHandlerRegistry>,
        artifacts: Arc<dyn TaskArtifactResolver>,
        execution_owner: impl Into<String>,
        failure_bus: DurableEventBus,
    ) -> Result<Self, EvaError> {
        Self::start_paused_with_timing_and_services(
            store,
            registry,
            artifacts,
            execution_owner,
            TaskWorkerTiming::default(),
            Some(failure_bus),
            None,
        )
    }

    /// Creates a paused production worker with failure replay and non-idempotent effect state.
    pub fn start_paused_with_durable_services(
        store: FileSystemTaskStateStore,
        registry: Arc<TaskHandlerRegistry>,
        artifacts: Arc<dyn TaskArtifactResolver>,
        execution_owner: impl Into<String>,
        failure_bus: DurableEventBus,
        effect_ledger: FileSystemEffectLedger,
    ) -> Result<Self, EvaError> {
        Self::start_paused_with_timing_and_services(
            store,
            registry,
            artifacts,
            execution_owner,
            TaskWorkerTiming::default(),
            Some(failure_bus),
            Some(effect_ledger),
        )
    }

    /// Creates a paused worker with explicit scan and heartbeat intervals.
    fn start_paused_with_timing(
        store: FileSystemTaskStateStore,
        registry: Arc<TaskHandlerRegistry>,
        artifacts: Arc<dyn TaskArtifactResolver>,
        execution_owner: impl Into<String>,
        timing: TaskWorkerTiming,
    ) -> Result<Self, EvaError> {
        Self::start_paused_with_timing_and_services(
            store,
            registry,
            artifacts,
            execution_owner,
            timing,
            None,
            None,
        )
    }

    fn start_paused_with_timing_and_services(
        store: FileSystemTaskStateStore,
        registry: Arc<TaskHandlerRegistry>,
        artifacts: Arc<dyn TaskArtifactResolver>,
        execution_owner: impl Into<String>,
        timing: TaskWorkerTiming,
        failure_bus: Option<DurableEventBus>,
        effect_ledger: Option<FileSystemEffectLedger>,
    ) -> Result<Self, EvaError> {
        let timing = timing.validate()?;
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
        let shared = Arc::new(TaskWorkerShared::new(timing));
        let replay_store = store.clone();
        let thread_shared = Arc::clone(&shared);
        let thread_registry = Arc::clone(&registry);
        let thread_artifacts = Arc::clone(&artifacts);
        let thread_execution_owner = execution_owner.clone();
        let join = thread::Builder::new()
            .name("eva-task-worker".to_owned())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    run_task_worker_loop(
                        store,
                        thread_registry,
                        thread_artifacts,
                        thread_execution_owner,
                        timing,
                        failure_bus,
                        effect_ledger,
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
            store: replay_store,
            execution_owner,
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

impl OwnedReplayHandler for TaskWorkerRuntime {
    fn reconcile_replay_delivery(
        &self,
        binding: &ReplayHandlerBinding,
        event: &Event,
        delivery_index: usize,
        retry_backoff_ms: u64,
        observed_at_ms: u64,
    ) -> Result<OwnedReplayDeliveryStatus, EvaError> {
        self.check_health()?;
        if !self.shared.claims_enabled() || self.shared.should_stop() {
            return Err(
                EvaError::unavailable("task worker is not accepting replay handler work")
                    .with_context("execution_owner", &self.execution_owner),
            );
        }
        let task_id = replay_delivery_task_id(event, binding, delivery_index)?;
        let idempotency_key =
            replay_delivery_idempotency_key(&self.store, binding, event, &task_id)?;
        let envelope = TaskEnvelope::new(
            TaskKind::parse(binding.handler_kind())?,
            binding.agent_id().clone(),
            TaskInput::inline(event_payload_bytes(event.payload()))?,
            idempotency_key,
            TaskAttemptPolicy::new(u32::MAX, retry_backoff_ms, None)?,
        )?;
        let expected_envelope = envelope.to_snapshot();
        let expected_queued = TaskStateSnapshot::queued_with_replay_delivery(
            task_id.as_str(),
            expected_envelope.clone(),
            event.event_id().as_str(),
            delivery_index,
        )?;
        let mut store = self.store.clone();
        for _ in 0..4 {
            let snapshot = match store.read(Some(task_id.as_str())) {
                Ok(snapshot) => snapshot,
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    match store.create(&expected_queued) {
                        Ok(snapshot) => {
                            self.notify_new_work();
                            snapshot
                        }
                        Err(error) if error.kind() == ErrorKind::Conflict => continue,
                        Err(error) => return Err(error),
                    }
                }
                Err(error) => return Err(error),
            };
            let envelope_matches = snapshot.envelope.as_ref() == Some(&expected_envelope)
                || snapshot.envelope.as_ref().is_some_and(|actual| {
                    legacy_replay_envelope_matches(binding, &task_id, actual, &expected_envelope)
                });
            if !envelope_matches {
                return Err(EvaError::conflict(
                    "replay delivery task envelope does not match its durable binding",
                )
                .with_context("task_id", task_id.as_str())
                .with_context("replay_event_id", event.event_id().as_str())
                .with_context("delivery_index", delivery_index.to_string()));
            }
            if snapshot.replay_delivery != expected_queued.replay_delivery {
                return Err(EvaError::conflict(
                    "replay delivery task is missing its durable replay identity",
                )
                .with_context("task_id", task_id.as_str())
                .with_context("replay_event_id", event.event_id().as_str())
                .with_context("delivery_index", delivery_index.to_string()));
            }
            match snapshot.status.as_str() {
                "queued" | "running" | "cancelling" => {
                    self.notify_new_work();
                    return Ok(OwnedReplayDeliveryStatus::Pending {
                        task_id: task_id.as_str().to_owned(),
                        status: snapshot.status,
                    });
                }
                "completed" => {
                    return Ok(OwnedReplayDeliveryStatus::Succeeded {
                        task_id: task_id.as_str().to_owned(),
                        result_digest: snapshot.result_digest.ok_or_else(|| {
                            EvaError::conflict(
                                "completed replay delivery is missing its result digest",
                            )
                            .with_context("task_id", task_id.as_str())
                        })?,
                        result_size_bytes: snapshot.result_size_bytes.ok_or_else(|| {
                            EvaError::conflict(
                                "completed replay delivery is missing its result size",
                            )
                            .with_context("task_id", task_id.as_str())
                        })?,
                    });
                }
                "failed" | "timed_out" => {
                    let error = task_snapshot_error(&snapshot)?;
                    let retryable =
                        error.is_retryable() && snapshot.attempts < snapshot.retry_max_attempts;
                    let was_deferred = snapshot.retry_ready_at_ms.is_some();
                    let retry_scheduled = if retryable {
                        let requeued = store
                            .requeue_retryable(task_id.as_str(), u128::from(observed_at_ms))?;
                        let scheduled = requeued.retry_ready_at_ms.is_some()
                            || matches!(requeued.status.as_str(), "queued" | "running");
                        if scheduled {
                            if matches!(requeued.status.as_str(), "queued" | "running") {
                                self.notify_new_work();
                            }
                            if was_deferred
                                && matches!(requeued.status.as_str(), "queued" | "running")
                            {
                                return Ok(OwnedReplayDeliveryStatus::Pending {
                                    task_id: task_id.as_str().to_owned(),
                                    status: requeued.status,
                                });
                            }
                        }
                        scheduled
                    } else {
                        false
                    };
                    return Ok(OwnedReplayDeliveryStatus::Failed {
                        task_id: task_id.as_str().to_owned(),
                        error,
                        retry_scheduled,
                    });
                }
                "interrupted" | "recovering" => {
                    let recovered = store.recover_abandoned_replay_delivery(task_id.as_str())?;
                    if recovered.status == "queued" {
                        self.notify_new_work();
                        return Ok(OwnedReplayDeliveryStatus::Pending {
                            task_id: task_id.as_str().to_owned(),
                            status: recovered.status,
                        });
                    }
                    return Ok(OwnedReplayDeliveryStatus::Failed {
                        task_id: task_id.as_str().to_owned(),
                        error: EvaError::conflict(
                            "replay delivery could not be recovered from its durable task state",
                        )
                        .with_context("status", recovered.status),
                        retry_scheduled: false,
                    });
                }
                "cancelled" => {
                    return Ok(OwnedReplayDeliveryStatus::Failed {
                        task_id: task_id.as_str().to_owned(),
                        error: EvaError::conflict(
                            "replay delivery cannot continue from its durable task state",
                        )
                        .with_context("status", &snapshot.status),
                        retry_scheduled: false,
                    });
                }
                status => {
                    return Err(EvaError::conflict(
                        "replay delivery task has an unsupported lifecycle status",
                    )
                    .with_context("task_id", task_id.as_str())
                    .with_context("status", status));
                }
            }
        }
        Err(
            EvaError::conflict("replay delivery reconciliation exceeded the CAS retry limit")
                .with_context("task_id", task_id.as_str()),
        )
    }
}

fn replay_delivery_task_id(
    event: &Event,
    binding: &ReplayHandlerBinding,
    delivery_index: usize,
) -> Result<RequestId, EvaError> {
    let material = format!(
        "{}\0{delivery_index}\0{}\0{}",
        event.event_id().as_str(),
        binding.handler_kind(),
        binding.agent_id().as_str()
    );
    let digest = sha256_digest(material.as_bytes());
    RequestId::parse(&format!(
        "replay-delivery-{}",
        digest.strip_prefix("sha256:").unwrap_or(&digest)
    ))
}

fn replay_delivery_idempotency_key(
    store: &FileSystemTaskStateStore,
    binding: &ReplayHandlerBinding,
    event: &Event,
    replay_task_id: &RequestId,
) -> Result<IdempotencyKey, EvaError> {
    if let Some(idempotency_key) = binding.idempotency_key() {
        return IdempotencyKey::parse(idempotency_key.as_str());
    }
    if event.topic().as_str() != "/runtime/task/failure" {
        return IdempotencyKey::parse(replay_task_id.as_str());
    }
    let Some(original_task_id) = event.metadata().request_id() else {
        return IdempotencyKey::parse(replay_task_id.as_str());
    };
    let original = match store.read(Some(original_task_id.as_str())) {
        Ok(snapshot) => snapshot,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return IdempotencyKey::parse(replay_task_id.as_str());
        }
        Err(error) => return Err(error),
    };
    let envelope = original.envelope.as_ref().ok_or_else(|| {
        EvaError::conflict("legacy task failure binding points to a task without an envelope")
            .with_context("task_id", &original.task_id)
    })?;
    let failure_event_id = task_failure_event_id(&original.task_id, original.attempts)?;
    let source_event_matches = event.event_id() == &failure_event_id
        || event.metadata().trace().causation_id() == Some(&failure_event_id);
    let dead_letter_matches = original.dead_letters.iter().any(|record| {
        record.event_id == failure_event_id.as_str()
            && record.topic == "/runtime/task/failure"
            && original.error_kind.as_deref() == Some(record.reason_kind.as_str())
            && original.error_message.as_deref() == Some(record.reason.as_str())
    });
    let target_matches = matches!(
        event.target(),
        EventTarget::Agent(agent_id) if agent_id.as_str() == envelope.agent_id
    );
    let payload_matches =
        sha256_digest(&event_payload_bytes(event.payload())) == envelope.input.digest();
    if original.replay_delivery.is_some()
        || !matches!(original.status.as_str(), "failed" | "timed_out")
        || original.error_retryable.is_none()
        || binding.handler_kind() != envelope.kind
        || binding.agent_id().as_str() != envelope.agent_id
        || !source_event_matches
        || !dead_letter_matches
        || !target_matches
        || !payload_matches
    {
        return Err(EvaError::conflict(
            "legacy task failure binding does not match its original task evidence",
        )
        .with_context("task_id", &original.task_id)
        .with_context("replay_event_id", event.event_id().as_str()));
    }
    IdempotencyKey::parse(&envelope.idempotency_key)
}

fn legacy_replay_envelope_matches(
    binding: &ReplayHandlerBinding,
    replay_task_id: &RequestId,
    actual: &eva_storage::TaskEnvelopeSnapshot,
    expected: &eva_storage::TaskEnvelopeSnapshot,
) -> bool {
    if binding.idempotency_key().is_some() || actual.idempotency_key != replay_task_id.as_str() {
        return false;
    }
    let mut upgraded = actual.clone();
    upgraded
        .idempotency_key
        .clone_from(&expected.idempotency_key);
    &upgraded == expected
}

fn event_payload_bytes(payload: &EventPayload) -> Vec<u8> {
    match payload {
        EventPayload::Empty => Vec::new(),
        EventPayload::Text(value) => value.as_bytes().to_vec(),
        EventPayload::Bytes(value) => value.clone(),
    }
}

fn task_snapshot_error(snapshot: &TaskStateSnapshot) -> Result<EvaError, EvaError> {
    let kind = match snapshot.error_kind.as_deref() {
        Some("invalid_argument") => ErrorKind::InvalidArgument,
        Some("not_found") => ErrorKind::NotFound,
        Some("conflict") => ErrorKind::Conflict,
        Some("permission_denied") => ErrorKind::PermissionDenied,
        Some("timeout") => ErrorKind::Timeout,
        Some("unavailable") => ErrorKind::Unavailable,
        Some("internal") => ErrorKind::Internal,
        Some("unsupported") => ErrorKind::Unsupported,
        Some(value) => {
            return Err(
                EvaError::conflict("replay delivery task has an invalid error kind")
                    .with_context("task_id", &snapshot.task_id)
                    .with_context("error_kind", value),
            )
        }
        None => {
            return Err(
                EvaError::conflict("failed replay delivery task is missing its error kind")
                    .with_context("task_id", &snapshot.task_id),
            )
        }
    };
    let message = snapshot.error_message.as_deref().ok_or_else(|| {
        EvaError::conflict("failed replay delivery task is missing its error message")
            .with_context("task_id", &snapshot.task_id)
    })?;
    Ok(EvaError::new(kind, message).with_retryable(
        snapshot
            .error_retryable
            .unwrap_or_else(|| kind.default_retryable()),
    ))
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

#[allow(clippy::too_many_arguments)]
fn run_task_worker_loop(
    mut store: FileSystemTaskStateStore,
    registry: Arc<TaskHandlerRegistry>,
    artifacts: Arc<dyn TaskArtifactResolver>,
    execution_owner: String,
    timing: TaskWorkerTiming,
    mut failure_bus: Option<DurableEventBus>,
    mut effect_ledger: Option<FileSystemEffectLedger>,
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
            if task_failure_evidence_event_id(&snapshot)?.is_some() {
                if let Some(bus) = failure_bus.as_mut() {
                    materialize_task_failure_evidence(
                        &mut store,
                        bus,
                        artifacts.as_ref(),
                        &snapshot,
                    )?;
                    claimed_any = true;
                }
                continue;
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
                timing.heartbeat_interval,
                failure_bus.as_mut(),
                effect_ledger.as_mut(),
            )?;
        }
        if !claimed_any {
            shared.wait_for_work();
        }
    }
}

/// A short-lived heartbeat thread owned by one synchronous handler attempt.
/// The handler trait is intentionally synchronous, so the durable lease must
/// be renewed independently while the handler is blocked.
struct TaskHeartbeatLoop {
    stop: Arc<(Mutex<bool>, Condvar)>,
    error: Arc<Mutex<Option<EvaError>>>,
    join: Option<JoinHandle<()>>,
}

impl TaskHeartbeatLoop {
    fn start(
        store: &FileSystemTaskStateStore,
        fence: &TaskAttemptFence,
        cancellation: TaskCancellationView,
        interval: Duration,
    ) -> Result<Self, EvaError> {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let error = Arc::new(Mutex::new(None));
        let thread_stop = Arc::clone(&stop);
        let thread_error = Arc::clone(&error);
        let mut heartbeat_store = store.clone();
        let heartbeat_fence = fence.clone();
        let join = thread::Builder::new()
            .name("eva-task-heartbeat".to_owned())
            .spawn(move || loop {
                let stopped = thread_stop
                    .0
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let (stopped, _) = thread_stop
                    .1
                    .wait_timeout_while(stopped, interval, |stopped| !*stopped)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if *stopped {
                    break;
                }
                drop(stopped);
                let now_ms = match worker_now_ms() {
                    Ok(value) => value,
                    Err(clock_error) => {
                        let mut slot = thread_error
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        *slot = Some(clock_error);
                        cancellation.request();
                        break;
                    }
                };
                match heartbeat_store.heartbeat_execution(&heartbeat_fence, now_ms) {
                    Ok(_) => {}
                    Err(heartbeat_error) => {
                        let mut slot = thread_error
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        *slot = Some(heartbeat_error);
                        cancellation.request();
                        break;
                    }
                }
            })
            .map_err(|error| {
                EvaError::internal("failed to start task heartbeat thread")
                    .with_context("io_error", error.to_string())
            })?;
        Ok(Self {
            stop,
            error,
            join: Some(join),
        })
    }

    fn stop_and_join(&mut self) -> Option<EvaError> {
        {
            let mut stopped = self
                .stop
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *stopped = true;
            self.stop.1.notify_all();
        }
        let join_error = self.join.take().and_then(|join| {
            join.join()
                .err()
                .map(|_| EvaError::internal("task heartbeat thread panicked"))
        });
        self.error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .or(join_error)
    }
}

impl Drop for TaskHeartbeatLoop {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_task_claim(
    store: &mut FileSystemTaskStateStore,
    registry: &TaskHandlerRegistry,
    artifacts: &dyn TaskArtifactResolver,
    shared: &TaskWorkerShared,
    claim: TaskExecutionClaim,
    heartbeat_interval: Duration,
    mut failure_bus: Option<&mut DurableEventBus>,
    mut effect_ledger: Option<&mut FileSystemEffectLedger>,
) -> Result<(), EvaError> {
    let task_id = RequestId::parse(&claim.snapshot().task_id)?;
    let cancel_token = claim.fence().cancel_token().to_owned();
    let cancellation = TaskCancellationView::default();
    shared.register_attempt(task_id.as_str(), &cancel_token, cancellation.clone())?;
    let mut heartbeat = match TaskHeartbeatLoop::start(
        store,
        claim.fence(),
        cancellation.clone(),
        heartbeat_interval,
    ) {
        Ok(heartbeat) => heartbeat,
        Err(error) => {
            shared.unregister_attempt(task_id.as_str(), &cancel_token);
            store.finish_execution(
                claim.fence(),
                &TaskAttemptOutcome::Failed {
                    error_kind: error.kind().as_str().to_owned(),
                    error_message: error.message().to_owned(),
                    retryable: error.is_retryable(),
                },
            )?;
            return Ok(());
        }
    };

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
        let decision = if cancel_precedes_dispatch {
            TaskDispatchDecision {
                outcome: TaskAttemptOutcome::Failed {
                    error_kind: ErrorKind::Conflict.as_str().to_owned(),
                    error_message: "task cancellation preceded handler dispatch".to_owned(),
                    retryable: false,
                },
                non_idempotent: false,
                effect_committed: false,
            }
        } else if claim.snapshot().deadline_expired(observed_before_dispatch) {
            TaskDispatchDecision {
                outcome: TaskAttemptOutcome::TimedOut {
                    observed_at_ms: observed_before_dispatch,
                    retryable: true,
                },
                non_idempotent: false,
                effect_committed: false,
            }
        } else {
            dispatch_claimed_attempt(
                store,
                registry,
                artifacts,
                &task_id,
                claim.snapshot(),
                claim.fence(),
                &cancellation,
                effect_ledger.as_deref_mut(),
            )?
        };
        let heartbeat_error = heartbeat.stop_and_join();
        let effect_committed = decision.effect_committed;
        let outcome = match heartbeat_error {
            Some(error) if !decision.non_idempotent => TaskAttemptOutcome::Failed {
                error_kind: error.kind().as_str().to_owned(),
                error_message: "task heartbeat persistence failed".to_owned(),
                retryable: error.is_retryable(),
            },
            _ => decision.outcome,
        };
        let finished = if effect_committed {
            match &outcome {
                TaskAttemptOutcome::Completed {
                    result_digest,
                    result_size_bytes,
                } => store.finish_committed_effect_execution(
                    claim.fence(),
                    result_digest,
                    *result_size_bytes,
                )?,
                _ => {
                    return Err(EvaError::internal(
                        "committed effect dispatch did not produce a completed outcome",
                    ))
                }
            }
        } else {
            store.finish_execution(claim.fence(), &outcome)?
        };
        if !latest.cancel_requested {
            if let Some(bus) = failure_bus.as_deref_mut() {
                materialize_task_failure_evidence(store, bus, artifacts, &finished)?;
            }
        }
        Ok(())
    })();

    shared.unregister_attempt(task_id.as_str(), &cancel_token);
    execution
}

fn task_failure_evidence_event_id(
    snapshot: &TaskStateSnapshot,
) -> Result<Option<EventId>, EvaError> {
    if snapshot.replay_delivery.is_some()
        || snapshot.envelope.is_none()
        || snapshot.error_retryable.is_none()
        || !matches!(snapshot.status.as_str(), "failed" | "timed_out")
    {
        return Ok(None);
    }
    let event_id = task_failure_event_id(&snapshot.task_id, snapshot.attempts)?;
    if let Some(existing) = snapshot
        .dead_letters
        .iter()
        .find(|record| record.event_id == event_id.as_str())
    {
        if existing.topic != "/runtime/task/failure"
            || snapshot.error_kind.as_deref() != Some(existing.reason_kind.as_str())
            || snapshot.error_message.as_deref() != Some(existing.reason.as_str())
        {
            return Err(EvaError::conflict(
                "task failure evidence summary does not match its terminal outcome",
            )
            .with_context("task_id", &snapshot.task_id)
            .with_context("event_id", event_id.as_str()));
        }
        return Ok(None);
    }
    Ok(Some(event_id))
}

fn materialize_task_failure_evidence(
    store: &mut FileSystemTaskStateStore,
    bus: &mut DurableEventBus,
    artifacts: &dyn TaskArtifactResolver,
    snapshot: &TaskStateSnapshot,
) -> Result<(), EvaError> {
    let Some(event_id) = task_failure_evidence_event_id(snapshot)? else {
        return Ok(());
    };
    let record = record_task_failure_dead_letter(bus, artifacts, snapshot, event_id)?;
    checkpoint_task_failure_evidence(store, snapshot, &record)
}

fn record_task_failure_dead_letter(
    bus: &mut DurableEventBus,
    artifacts: &dyn TaskArtifactResolver,
    snapshot: &TaskStateSnapshot,
    event_id: EventId,
) -> Result<DeadLetterRecord, EvaError> {
    let reason = task_snapshot_error(snapshot)?;
    let envelope = snapshot
        .envelope
        .clone()
        .ok_or_else(|| EvaError::internal("failed task is missing its durable envelope"))
        .and_then(TaskEnvelope::try_from)?;
    let payload = resolve_task_payload(&envelope, artifacts)
        .ok()
        .map(|(bytes, _)| bytes);
    let topic = Topic::parse("/runtime/task/failure")?;
    let event = Event::new(
        event_id.clone(),
        topic,
        payload
            .as_ref()
            .map(|bytes| EventPayload::bytes(bytes.clone()))
            .unwrap_or_else(EventPayload::empty),
    )
    .with_target(EventTarget::Agent(envelope.agent_id().clone()))
    .with_request_id(RequestId::parse(&snapshot.task_id)?);
    ensure_task_failure_event(bus, &event)?;
    let now_ms = u64::try_from(worker_now_ms()?).unwrap_or(u64::MAX);
    let redrive = RedrivePolicy {
        retry_delay_ms: envelope.attempt_policy().retry_backoff_ms,
        next_attempt_after_ms: now_ms.saturating_add(envelope.attempt_policy().retry_backoff_ms),
    };
    let expected_bindings = if payload.is_some() {
        vec![
            ReplayHandlerBinding::new(envelope.kind().as_str(), envelope.agent_id().clone())?
                .with_idempotency_key(RequestId::parse(envelope.idempotency_key().as_str())?),
        ]
    } else {
        Vec::new()
    };
    if let Some(existing) = bus
        .dead_letters()
        .iter()
        .find(|record| record.event_id() == &event_id)
        .cloned()
    {
        validate_task_failure_dead_letter(
            &existing,
            &event,
            &reason,
            &expected_bindings,
            redrive.retry_delay_ms,
        )?;
        return Ok(existing);
    }
    let result = if expected_bindings.is_empty() {
        bus.dead_letter_with_redrive(event.clone(), reason.clone(), redrive)
    } else {
        bus.dead_letter_for_handlers_with_redrive(
            event.clone(),
            reason.clone(),
            expected_bindings.clone(),
            redrive,
        )
    };
    match result {
        Ok(record) => Ok(record),
        Err(error) if error.kind() == ErrorKind::Conflict => {
            bus.refresh_dead_letters()?;
            let Some(existing) = bus
                .dead_letters()
                .iter()
                .find(|record| record.event_id() == &event_id)
                .cloned()
            else {
                return Err(error);
            };
            validate_task_failure_dead_letter(
                &existing,
                &event,
                &reason,
                &expected_bindings,
                redrive.retry_delay_ms,
            )?;
            Ok(existing)
        }
        Err(error) => Err(error),
    }
}

fn ensure_task_failure_event(bus: &mut DurableEventBus, event: &Event) -> Result<(), EvaError> {
    if let Some(existing) = bus.event_log_record(event.event_id()) {
        return validate_task_failure_event(existing, event);
    }
    match bus.publish(event.clone()) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == ErrorKind::Conflict => {
            bus.refresh_event_log()?;
            let Some(existing) = bus.event_log_record(event.event_id()) else {
                return Err(error);
            };
            validate_task_failure_event(existing, event)
        }
        Err(error) => Err(error),
    }
}

fn validate_task_failure_event(
    existing: &eva_storage::EventLogRecord,
    expected: &Event,
) -> Result<(), EvaError> {
    validate_task_failure_event_identity(&existing.event, expected)
}

fn validate_task_failure_event_identity(
    existing: &Event,
    expected: &Event,
) -> Result<(), EvaError> {
    if existing.topic() == expected.topic()
        && existing.target() == expected.target()
        && existing.payload() == expected.payload()
        && existing.metadata().request_id() == expected.metadata().request_id()
    {
        return Ok(());
    }
    Err(
        EvaError::conflict("task failure event identity collides with another durable event")
            .with_context("event_id", expected.event_id().as_str()),
    )
}

fn validate_task_failure_dead_letter(
    existing: &DeadLetterRecord,
    expected_event: &Event,
    expected_reason: &EvaError,
    expected_bindings: &[ReplayHandlerBinding],
    expected_retry_delay_ms: u64,
) -> Result<(), EvaError> {
    if validate_task_failure_event_identity(&existing.event, expected_event).is_ok()
        && replay_handler_bindings_match_with_legacy_keys(
            &existing.replay_handlers,
            expected_bindings,
        )
        && existing.reason.kind() == expected_reason.kind()
        && existing.reason.message() == expected_reason.message()
        && existing.reason.is_retryable() == expected_reason.is_retryable()
        && existing.redrive.retry_delay_ms == expected_retry_delay_ms
    {
        return Ok(());
    }
    Err(
        EvaError::conflict("task failure dead-letter identity collides with another record")
            .with_context("event_id", expected_event.event_id().as_str()),
    )
}

fn replay_handler_bindings_match_with_legacy_keys(
    existing: &[ReplayHandlerBinding],
    expected: &[ReplayHandlerBinding],
) -> bool {
    existing.len() == expected.len()
        && existing.iter().zip(expected).all(|(existing, expected)| {
            existing.handler_kind() == expected.handler_kind()
                && existing.agent_id() == expected.agent_id()
                && (existing.idempotency_key() == expected.idempotency_key()
                    || (existing.idempotency_key().is_none()
                        && expected.idempotency_key().is_some()))
        })
}

fn checkpoint_task_failure_evidence(
    store: &mut FileSystemTaskStateStore,
    original: &TaskStateSnapshot,
    record: &DeadLetterRecord,
) -> Result<(), EvaError> {
    let summary = TaskStateDeadLetterSnapshot {
        event_id: record.event_id().as_str().to_owned(),
        topic: record.event.topic().as_str().to_owned(),
        reason_kind: record.reason.kind().as_str().to_owned(),
        reason: record.reason.message().to_owned(),
        replay_count: record.replay_count,
    };
    for _ in 0..4 {
        let mut current = store.read(Some(&original.task_id))?;
        if let Some(existing) = current
            .dead_letters
            .iter()
            .find(|existing| existing.event_id == summary.event_id)
        {
            if existing == &summary {
                return Ok(());
            }
            return Err(EvaError::conflict(
                "task failure evidence identity collides with another summary",
            )
            .with_context("task_id", &original.task_id)
            .with_context("event_id", &summary.event_id));
        }
        if !matches!(current.status.as_str(), "failed" | "timed_out")
            || current.attempts != original.attempts
            || current.error_retryable.is_none()
        {
            return Err(EvaError::conflict(
                "task failure outcome changed before evidence checkpoint",
            )
            .with_context("task_id", &original.task_id)
            .with_context("status", &current.status));
        }
        current.dead_letters.push(summary.clone());
        current.push_log(
            "info",
            format!("task failure evidence committed: {}", summary.event_id),
        );
        match store.compare_and_set(&current) {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == ErrorKind::Conflict => continue,
            Err(error) => return Err(error),
        }
    }
    Err(
        EvaError::conflict("task failure evidence checkpoint exceeded the CAS retry limit")
            .with_context("task_id", &original.task_id),
    )
}

fn task_failure_event_id(task_id: &str, attempt: usize) -> Result<EventId, EvaError> {
    let digest = sha256_digest(format!("{task_id}\0{attempt}").as_bytes());
    let hex = digest.strip_prefix("sha256:").unwrap_or(&digest);
    EventId::parse(&format!("task-failure-{}", &hex[..32]))
}

#[allow(clippy::too_many_arguments)]
fn dispatch_claimed_attempt(
    task_store: &FileSystemTaskStateStore,
    registry: &TaskHandlerRegistry,
    artifacts: &dyn TaskArtifactResolver,
    task_id: &RequestId,
    snapshot: &eva_storage::TaskStateSnapshot,
    fence: &TaskAttemptFence,
    cancellation: &TaskCancellationView,
    effect_ledger: Option<&mut FileSystemEffectLedger>,
) -> Result<TaskDispatchDecision, EvaError> {
    let envelope = snapshot
        .envelope
        .clone()
        .ok_or_else(|| EvaError::internal("claimed task is missing its envelope"))
        .and_then(TaskEnvelope::try_from)?;
    let registration = registry.handlers.get(envelope.kind());
    let non_idempotent = registration.is_some_and(|entry| entry.effect_scope.is_some());
    let dispatched = match registration {
        Some(registration) if registration.effect_scope.is_some() => {
            dispatch_non_idempotent_attempt(
                task_store,
                registration,
                registration.effect_scope.as_deref().unwrap_or_default(),
                artifacts,
                task_id,
                &envelope,
                snapshot,
                fence,
                cancellation,
                effect_ledger,
            )
        }
        Some(_) => catch_unwind(AssertUnwindSafe(|| {
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
        .map(ClaimedDispatchResult::Executed),
        None => Err(EvaError::not_found(TASK_HANDLER_NOT_REGISTERED_MESSAGE)
            .with_context("task_id", task_id.as_str())
            .with_context("task_kind", envelope.kind().as_str())
            .with_context("agent_id", envelope.agent_id().as_str())),
    };
    let observed_at_ms = worker_now_ms()?;
    if !non_idempotent && snapshot.deadline_expired(observed_at_ms) {
        return Ok(TaskDispatchDecision {
            outcome: TaskAttemptOutcome::TimedOut {
                observed_at_ms,
                retryable: true,
            },
            non_idempotent,
            effect_committed: false,
        });
    }
    let (outcome, effect_committed) = match dispatched {
        Ok(ClaimedDispatchResult::Executed(result)) => (
            TaskAttemptOutcome::Completed {
                result_digest: result.digest().to_owned(),
                result_size_bytes: result.size_bytes(),
            },
            false,
        ),
        Ok(ClaimedDispatchResult::EffectCommitted {
            result_digest,
            result_size_bytes,
        }) => (
            TaskAttemptOutcome::Completed {
                result_digest,
                result_size_bytes,
            },
            true,
        ),
        Err(error) if error.kind() == ErrorKind::Timeout => (
            TaskAttemptOutcome::TimedOut {
                observed_at_ms,
                retryable: error.is_retryable(),
            },
            false,
        ),
        Err(error) => (
            TaskAttemptOutcome::Failed {
                error_kind: error.kind().as_str().to_owned(),
                error_message: error.message().to_owned(),
                retryable: error.is_retryable(),
            },
            false,
        ),
    };
    Ok(TaskDispatchDecision {
        outcome,
        non_idempotent,
        effect_committed,
    })
}

struct TaskDispatchDecision {
    outcome: TaskAttemptOutcome,
    non_idempotent: bool,
    effect_committed: bool,
}

enum ClaimedDispatchResult {
    Executed(TaskHandlerResult),
    EffectCommitted {
        result_digest: String,
        result_size_bytes: usize,
    },
}

#[allow(clippy::too_many_arguments)]
fn dispatch_non_idempotent_attempt(
    task_store: &FileSystemTaskStateStore,
    registration: &TaskHandlerRegistration,
    effect_scope: &str,
    artifacts: &dyn TaskArtifactResolver,
    task_id: &RequestId,
    envelope: &TaskEnvelope,
    snapshot: &TaskStateSnapshot,
    fence: &TaskAttemptFence,
    cancellation: &TaskCancellationView,
    effect_ledger: Option<&mut FileSystemEffectLedger>,
) -> Result<ClaimedDispatchResult, EvaError> {
    let effect_ledger = effect_ledger.ok_or_else(|| {
        EvaError::unavailable("non-idempotent task handler requires a durable effect ledger")
            .with_retryable(false)
            .with_context("task_id", task_id.as_str())
            .with_context("task_kind", envelope.kind().as_str())
            .with_context("agent_id", envelope.agent_id().as_str())
    })?;
    if snapshot.replay_delivery.is_some() && envelope.idempotency_key().as_str() == task_id.as_str()
    {
        return Err(EvaError::conflict(
            "non-idempotent replay delivery is missing its original idempotency key",
        )
        .with_retryable(false)
        .with_context("task_id", task_id.as_str())
        .with_context("task_kind", envelope.kind().as_str()));
    }
    let operation = EffectOperationIdentity::new(
        envelope.idempotency_key().as_str(),
        envelope.kind().as_str(),
        envelope.agent_id().as_str(),
        effect_scope,
        envelope.input().digest(),
    )?;
    let inspection_intent = effect_intent(operation.clone(), fence, worker_now_ms()?)?;
    if let Some(record) = effect_ledger.inspect_for_claim(task_store, &inspection_intent)? {
        return dispatch_from_effect_record(record);
    }

    // Payload validation remains before prepare so a missing/corrupt artifact cannot strand a
    // record that definitely never reached the handler. Committed replays bypass this I/O above.
    let (payload, payload_digest) = resolve_task_payload(envelope, artifacts).map_err(|error| {
        error
            .with_context("task_id", task_id.as_str())
            .with_context("agent_id", envelope.agent_id().as_str())
    })?;
    let intent = effect_intent(operation, fence, worker_now_ms()?)?;
    match effect_ledger.prepare_for_claim(task_store, &intent)? {
        EffectPrepareOutcome::Created(_) => {}
        EffectPrepareOutcome::Prepared(record) => {
            return Err(unresolved_effect_error(&record));
        }
        EffectPrepareOutcome::Committed(record) => {
            return dispatch_from_effect_record(record);
        }
    }

    if cancellation.is_requested() {
        return Err(prepared_effect_error(
            EvaError::conflict("task cancellation followed the effect prepare boundary")
                .with_retryable(false),
            intent.operation(),
        ));
    }

    let invocation = TaskHandlerInvocation {
        task_id,
        envelope,
        payload: &payload,
        payload_digest: &payload_digest,
        attempt: snapshot.attempts,
        deadline_at_ms: snapshot.deadline_at_ms,
        cancellation,
    };
    let result = catch_unwind(AssertUnwindSafe(|| {
        registration.handler.handle(&invocation)
    }))
    .unwrap_or_else(|_| Err(EvaError::internal(TASK_HANDLER_PANIC_MESSAGE)))
    .map_err(|error| prepared_effect_error(error, intent.operation()))?;
    effect_ledger
        .commit(
            &intent,
            result.digest(),
            result.size_bytes(),
            worker_now_ms()?,
        )
        .map_err(|error| prepared_effect_error(error, intent.operation()))?;
    Ok(ClaimedDispatchResult::EffectCommitted {
        result_digest: result.digest().to_owned(),
        result_size_bytes: result.size_bytes(),
    })
}

fn effect_intent(
    operation: EffectOperationIdentity,
    fence: &TaskAttemptFence,
    prepared_at_ms: u128,
) -> Result<EffectLedgerIntent, EvaError> {
    EffectLedgerIntent::new(
        operation,
        fence.task_id(),
        fence.owner_generation(),
        fence.execution_owner(),
        fence.attempt(),
        fence.cancel_token(),
        prepared_at_ms,
    )
}

fn dispatch_from_effect_record(
    record: EffectLedgerRecord,
) -> Result<ClaimedDispatchResult, EvaError> {
    if record.state() == EffectLedgerState::Prepared {
        return Err(unresolved_effect_error(&record));
    }
    let result_digest = record.result_digest().ok_or_else(|| {
        EvaError::conflict("committed effect is missing its result digest")
            .with_context("operation_digest", record.operation().operation_digest())
    })?;
    let result_size_bytes = record.result_size_bytes().ok_or_else(|| {
        EvaError::conflict("committed effect is missing its result size")
            .with_context("operation_digest", record.operation().operation_digest())
    })?;
    Ok(ClaimedDispatchResult::EffectCommitted {
        result_digest: result_digest.to_owned(),
        result_size_bytes,
    })
}

fn unresolved_effect_error(record: &EffectLedgerRecord) -> EvaError {
    EvaError::conflict("non-idempotent effect outcome is unresolved")
        .with_retryable(false)
        .with_context("effect_state", record.state().as_str())
        .with_context("operation_digest", record.operation().operation_digest())
}

fn prepared_effect_error(error: EvaError, operation: &EffectOperationIdentity) -> EvaError {
    error
        .with_retryable(false)
        .with_context("effect_state", EffectLedgerState::Prepared.as_str())
        .with_context("operation_digest", operation.operation_digest())
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
    use eva_core::{AgentId, ErrorKind, EventId, EventPayload, Topic};
    use std::fs;
    use std::io::Write;
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
    fn non_idempotent_handler_requires_durable_ledger_before_invocation() {
        let calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = Arc::clone(&calls);
        let mut registry = TaskHandlerRegistry::new();
        registry
            .register_non_idempotent(
                TaskKind::parse("vendor.side-effect").unwrap(),
                "vendor.side-effect.v1",
                move |_: &TaskHandlerInvocation<'_>| {
                    handler_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(TaskHandlerResult::new(b"effect-result".as_slice()))
                },
            )
            .unwrap();
        let task = TaskEnvelope::new(
            TaskKind::parse("vendor.side-effect").unwrap(),
            AgentId::parse("root-agent").unwrap(),
            TaskInput::inline(b"effect-input".as_slice()).unwrap(),
            IdempotencyKey::parse("idem-effect-direct").unwrap(),
            TaskAttemptPolicy::new(1, 0, None).unwrap(),
        )
        .unwrap();

        let error = registry
            .dispatch(&task_id(), &task, &InMemoryArtifactStore::new())
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert!(!error.is_retryable());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn committed_effect_is_reused_after_writer_restart_without_reinvoking_handler() {
        let root = test_root("effect-commit-restart");
        let effect_sink = root.path().join("effect-sink.log");
        let calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = Arc::clone(&calls);
        let handler_sink = effect_sink.clone();
        let mut registry = TaskHandlerRegistry::new();
        registry
            .register_non_idempotent(
                TaskKind::parse("vendor.side-effect").unwrap(),
                "vendor.side-effect.v1",
                move |_: &TaskHandlerInvocation<'_>| {
                    handler_calls.fetch_add(1, Ordering::SeqCst);
                    let mut sink = fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&handler_sink)
                        .unwrap();
                    writeln!(sink, "applied").unwrap();
                    sink.sync_all().unwrap();
                    Ok(TaskHandlerResult::new(b"effect-result".as_slice()))
                },
            )
            .unwrap();
        let registry = Arc::new(registry);
        let envelope = TaskEnvelope::new(
            TaskKind::parse("vendor.side-effect").unwrap(),
            AgentId::parse("root-agent").unwrap(),
            TaskInput::inline(b"effect-input".as_slice()).unwrap(),
            IdempotencyKey::parse("idem-effect-restart").unwrap(),
            TaskAttemptPolicy::new(1, 0, None).unwrap(),
        )
        .unwrap();
        let expected_digest = sha256_digest(b"effect-result");

        // Commit the ledger result but deliberately leave the first task running, matching a crash
        // after the external effect and ledger commit but before TaskState finish.
        {
            let backend = eva_storage::FileSystemDurableBackend::open(
                eva_storage::DurableBackendOptions::read_write(root.path()),
            )
            .unwrap();
            let writer = backend.acquire_runtime_writer().unwrap();
            let mut store =
                FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
                    .unwrap();
            store
                .create(
                    &TaskStateSnapshot::queued_with_envelope(
                        "req-effect-before-crash",
                        envelope.to_snapshot(),
                    )
                    .unwrap(),
                )
                .unwrap();
            let claim = store
                .try_claim_queued(
                    "req-effect-before-crash",
                    "daemon:effect:g1",
                    "cancel:effect:g1",
                    100,
                )
                .unwrap()
                .unwrap();
            let mut ledger =
                FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();
            let outcome = dispatch_claimed_attempt(
                &store,
                registry.as_ref(),
                &InMemoryArtifactStore::new(),
                &RequestId::parse("req-effect-before-crash").unwrap(),
                claim.snapshot(),
                claim.fence(),
                &TaskCancellationView::default(),
                Some(&mut ledger),
            )
            .unwrap();
            assert_eq!(
                outcome.outcome,
                TaskAttemptOutcome::Completed {
                    result_digest: expected_digest.clone(),
                    result_size_bytes: b"effect-result".len(),
                }
            );
            assert!(outcome.non_idempotent);
            assert!(outcome.effect_committed);
            assert_eq!(
                ledger
                    .inspect(
                        &EffectOperationIdentity::new(
                            "idem-effect-restart",
                            "vendor.side-effect",
                            "root-agent",
                            "vendor.side-effect.v1",
                            envelope.input().digest(),
                        )
                        .unwrap(),
                    )
                    .unwrap()
                    .unwrap()
                    .state(),
                EffectLedgerState::Committed
            );
            assert_eq!(
                store.read(Some("req-effect-before-crash")).unwrap().status,
                "running"
            );
        }
        assert_eq!(fs::read_to_string(&effect_sink).unwrap().lines().count(), 1);

        let backend = eva_storage::FileSystemDurableBackend::open(
            eva_storage::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut store =
            FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
                .unwrap();
        store
            .create(
                &TaskStateSnapshot::queued_with_envelope(
                    "req-effect-after-restart",
                    envelope.to_snapshot(),
                )
                .unwrap(),
            )
            .unwrap();
        let ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let mut worker = TaskWorkerRuntime::start_paused_with_timing_and_services(
            store.clone(),
            registry,
            Arc::new(InMemoryArtifactStore::new()),
            "daemon:effect:g2",
            TaskWorkerTiming::default(),
            None,
            Some(ledger),
        )
        .unwrap();
        worker.activate();
        let completed = wait_for_task_status(&store, "req-effect-after-restart", "completed");
        worker.stop_and_join().unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(fs::read_to_string(&effect_sink).unwrap().lines().count(), 1);
        assert_eq!(
            completed.result_digest.as_deref(),
            Some(expected_digest.as_str())
        );
        assert_eq!(completed.result_size_bytes, Some(b"effect-result".len()));
        assert_eq!(completed.owner_generation, writer.generation());
        assert_eq!(
            store.read(Some("req-effect-before-crash")).unwrap().status,
            "running"
        );
    }

    #[test]
    fn two_workers_with_one_business_key_invoke_effect_handler_once() {
        let root = test_root("effect-two-workers-one-key");
        let backend = eva_storage::FileSystemDurableBackend::open(
            eva_storage::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut store =
            FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
                .unwrap();
        let envelope = TaskEnvelope::new(
            TaskKind::parse("vendor.concurrent-effect").unwrap(),
            AgentId::parse("root-agent").unwrap(),
            TaskInput::inline(b"shared-effect-input".as_slice()).unwrap(),
            IdempotencyKey::parse("idem-concurrent-effect").unwrap(),
            TaskAttemptPolicy::new(1, 0, None).unwrap(),
        )
        .unwrap();
        for task_id in ["req-concurrent-effect-a", "req-concurrent-effect-b"] {
            store
                .create(
                    &TaskStateSnapshot::queued_with_envelope(task_id, envelope.to_snapshot())
                        .unwrap(),
                )
                .unwrap();
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let handler_started = Arc::new(AtomicBool::new(false));
        let handler_release = Arc::new(AtomicBool::new(false));
        let calls_by_handler = Arc::clone(&calls);
        let started_by_handler = Arc::clone(&handler_started);
        let release_by_handler = Arc::clone(&handler_release);
        let mut registry = TaskHandlerRegistry::new();
        registry
            .register_non_idempotent(
                TaskKind::parse("vendor.concurrent-effect").unwrap(),
                "vendor.concurrent-effect.v1",
                move |_: &TaskHandlerInvocation<'_>| {
                    calls_by_handler.fetch_add(1, Ordering::SeqCst);
                    started_by_handler.store(true, Ordering::Release);
                    let started_at = Instant::now();
                    while !release_by_handler.load(Ordering::Acquire) {
                        assert!(started_at.elapsed() < Duration::from_secs(5));
                        thread::yield_now();
                    }
                    Ok(TaskHandlerResult::new(b"shared-effect-result".as_slice()))
                },
            )
            .unwrap();
        let registry = Arc::new(registry);
        let timing =
            TaskWorkerTiming::new(Duration::from_millis(5), Duration::from_millis(10)).unwrap();
        let first_ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let second_ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let mut first = TaskWorkerRuntime::start_paused_with_timing_and_services(
            store.clone(),
            Arc::clone(&registry),
            Arc::new(InMemoryArtifactStore::new()),
            "daemon:effect:concurrent:first",
            timing,
            None,
            Some(first_ledger),
        )
        .unwrap();
        let mut second = TaskWorkerRuntime::start_paused_with_timing_and_services(
            store.clone(),
            registry,
            Arc::new(InMemoryArtifactStore::new()),
            "daemon:effect:concurrent:second",
            timing,
            None,
            Some(second_ledger),
        )
        .unwrap();
        first.activate();
        second.activate();
        wait_until(Duration::from_secs(2), || {
            handler_started.load(Ordering::Acquire)
        });

        let wait_started = Instant::now();
        loop {
            let statuses = [
                store.read(Some("req-concurrent-effect-a")).unwrap().status,
                store.read(Some("req-concurrent-effect-b")).unwrap().status,
            ];
            if statuses.iter().any(|status| status == "failed") {
                break;
            }
            if wait_started.elapsed() >= Duration::from_secs(2) {
                handler_release.store(true, Ordering::Release);
                let _ = first.stop_and_join();
                let _ = second.stop_and_join();
                panic!("competing effect task did not observe the prepared boundary");
            }
            thread::sleep(Duration::from_millis(5));
        }
        handler_release.store(true, Ordering::Release);

        let wait_started = Instant::now();
        let final_records = loop {
            let records = [
                store.read(Some("req-concurrent-effect-a")).unwrap(),
                store.read(Some("req-concurrent-effect-b")).unwrap(),
            ];
            if records.iter().any(|record| record.status == "completed") {
                break records;
            }
            assert!(wait_started.elapsed() < Duration::from_secs(2));
            thread::sleep(Duration::from_millis(5));
        };
        first.stop_and_join().unwrap();
        second.stop_and_join().unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            final_records
                .iter()
                .filter(|record| record.status == "completed")
                .count(),
            1
        );
        let failed = final_records
            .iter()
            .find(|record| record.status == "failed")
            .unwrap();
        assert_eq!(failed.error_retryable, Some(false));
        assert_eq!(failed.error_kind.as_deref(), Some("conflict"));
        let mut reopened = FileSystemEffectLedger::open_read_only(backend.layout()).unwrap();
        let operation = EffectOperationIdentity::new(
            "idem-concurrent-effect",
            "vendor.concurrent-effect",
            "root-agent",
            "vendor.concurrent-effect.v1",
            envelope.input().digest(),
        )
        .unwrap();
        assert_eq!(
            reopened.inspect(&operation).unwrap().unwrap().state(),
            EffectLedgerState::Committed
        );
    }

    #[test]
    fn prepared_effect_blocks_automatic_reexecution_and_stays_non_retryable() {
        let root = test_root("effect-prepared-block");
        let backend = eva_storage::FileSystemDurableBackend::open(
            eva_storage::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut store =
            FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
                .unwrap();
        let envelope = TaskEnvelope::new(
            TaskKind::parse("vendor.uncertain-effect").unwrap(),
            AgentId::parse("root-agent").unwrap(),
            TaskInput::inline(b"uncertain-input".as_slice()).unwrap(),
            IdempotencyKey::parse("idem-effect-uncertain").unwrap(),
            TaskAttemptPolicy::new(3, 0, None).unwrap(),
        )
        .unwrap();
        store
            .create(
                &TaskStateSnapshot::queued_with_envelope(
                    "req-effect-uncertain-first",
                    envelope.to_snapshot(),
                )
                .unwrap(),
            )
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = Arc::clone(&calls);
        let mut registry = TaskHandlerRegistry::new();
        registry
            .register_non_idempotent(
                TaskKind::parse("vendor.uncertain-effect").unwrap(),
                "vendor.uncertain-effect.v1",
                move |_: &TaskHandlerInvocation<'_>| {
                    handler_calls.fetch_add(1, Ordering::SeqCst);
                    Err(EvaError::unavailable("effect response was lost"))
                },
            )
            .unwrap();
        let ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let mut worker = TaskWorkerRuntime::start_paused_with_timing_and_services(
            store.clone(),
            Arc::new(registry),
            Arc::new(InMemoryArtifactStore::new()),
            "daemon:effect:uncertain",
            TaskWorkerTiming::default(),
            None,
            Some(ledger),
        )
        .unwrap();
        worker.activate();
        let first = wait_for_task_status(&store, "req-effect-uncertain-first", "failed");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(first.error_retryable, Some(false));
        assert_eq!(
            store
                .requeue_retryable("req-effect-uncertain-first", worker_now_ms().unwrap())
                .unwrap()
                .status,
            "failed"
        );

        store
            .create(
                &TaskStateSnapshot::queued_with_envelope(
                    "req-effect-uncertain-second",
                    envelope.to_snapshot(),
                )
                .unwrap(),
            )
            .unwrap();
        worker.notify_new_work();
        let second = wait_for_task_status(&store, "req-effect-uncertain-second", "failed");
        worker.stop_and_join().unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(second.error_kind.as_deref(), Some("conflict"));
        assert_eq!(
            second.error_message.as_deref(),
            Some("non-idempotent effect outcome is unresolved")
        );
        assert_eq!(second.error_retryable, Some(false));
        let mut reopened = FileSystemEffectLedger::open_read_only(backend.layout()).unwrap();
        let operation = EffectOperationIdentity::new(
            "idem-effect-uncertain",
            "vendor.uncertain-effect",
            "root-agent",
            "vendor.uncertain-effect.v1",
            envelope.input().digest(),
        )
        .unwrap();
        assert_eq!(
            reopened.inspect(&operation).unwrap().unwrap().state(),
            EffectLedgerState::Prepared
        );
    }

    #[test]
    fn cancellation_before_prepare_never_invokes_non_idempotent_handler() {
        let root = test_root("effect-cancel-before-prepare");
        let backend = eva_storage::FileSystemDurableBackend::open(
            eva_storage::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut store =
            FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
                .unwrap();
        let payload = b"cancel-before-prepare".to_vec();
        let digest = sha256_digest(&payload);
        let artifact_ref =
            TaskArtifactRef::new("tasks/cancel-before-prepare", digest.clone()).unwrap();
        let envelope = TaskEnvelope::new(
            TaskKind::parse("vendor.cancel-before-prepare").unwrap(),
            AgentId::parse("root-agent").unwrap(),
            TaskInput::artifact(artifact_ref.clone()),
            IdempotencyKey::parse("idem-cancel-before-prepare").unwrap(),
            TaskAttemptPolicy::new(1, 0, None).unwrap(),
        )
        .unwrap();
        store
            .create(
                &TaskStateSnapshot::queued_with_envelope(
                    "req-cancel-before-prepare",
                    envelope.to_snapshot(),
                )
                .unwrap(),
            )
            .unwrap();

        let handler_calls = Arc::new(AtomicUsize::new(0));
        let calls_by_handler = Arc::clone(&handler_calls);
        let mut registry = TaskHandlerRegistry::new();
        registry
            .register_non_idempotent(
                TaskKind::parse("vendor.cancel-before-prepare").unwrap(),
                "vendor.cancel-before-prepare.v1",
                move |_: &TaskHandlerInvocation<'_>| {
                    calls_by_handler.fetch_add(1, Ordering::SeqCst);
                    Ok(TaskHandlerResult::new(b"unexpected".as_slice()))
                },
            )
            .unwrap();
        let resolver_started = Arc::new(AtomicBool::new(false));
        let resolver_release = Arc::new(AtomicBool::new(false));
        let resolver: Arc<dyn TaskArtifactResolver> = Arc::new(BlockingResolver {
            record: ArtifactRecord {
                key: artifact_ref.key().to_owned(),
                bytes: payload.clone(),
                digest,
                size_bytes: payload.len(),
                content_type: "application/octet-stream".to_owned(),
                retention_policy: "retain".to_owned(),
                retain_until_ms: None,
            },
            started: Arc::clone(&resolver_started),
            release: Arc::clone(&resolver_release),
        });
        let ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let mut worker = TaskWorkerRuntime::start_paused_with_timing_and_services(
            store.clone(),
            Arc::new(registry),
            resolver,
            "daemon:effect:cancel-before-prepare",
            TaskWorkerTiming::new(Duration::from_millis(5), Duration::from_millis(10)).unwrap(),
            None,
            Some(ledger),
        )
        .unwrap();
        worker.activate();
        wait_until(Duration::from_secs(2), || {
            resolver_started.load(Ordering::Acquire)
        });
        let cancelling = store
            .request_cancellation("req-cancel-before-prepare", "operator cancellation")
            .unwrap();
        assert!(worker.signal_cancellation(
            "req-cancel-before-prepare",
            cancelling.cancel_token.as_deref()
        ));
        resolver_release.store(true, Ordering::Release);

        let cancelled = wait_for_task_status(&store, "req-cancel-before-prepare", "cancelled");
        worker.stop_and_join().unwrap();
        assert!(cancelled.cancel_accepted);
        assert_eq!(handler_calls.load(Ordering::SeqCst), 0);
        assert!(FileSystemEffectLedger::open_read_only(backend.layout())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn committed_effect_outweighs_late_cancellation_and_deadline() {
        let root = test_root("effect-commit-race");
        let backend = eva_storage::FileSystemDurableBackend::open(
            eva_storage::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut store =
            FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
                .unwrap();
        let envelope = TaskEnvelope::new(
            TaskKind::parse("vendor.commit-race").unwrap(),
            AgentId::parse("root-agent").unwrap(),
            TaskInput::inline(b"commit-race-input".as_slice()).unwrap(),
            IdempotencyKey::parse("idem-effect-commit-race").unwrap(),
            TaskAttemptPolicy::new(1, 0, Some(500)).unwrap(),
        )
        .unwrap();
        store
            .create(
                &TaskStateSnapshot::queued_with_envelope(
                    "req-effect-commit-race",
                    envelope.to_snapshot(),
                )
                .unwrap(),
            )
            .unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let handler_started = Arc::new(AtomicBool::new(false));
        let handler_release = Arc::new(AtomicBool::new(false));
        let calls_by_handler = Arc::clone(&calls);
        let started_by_handler = Arc::clone(&handler_started);
        let release_by_handler = Arc::clone(&handler_release);
        let mut registry = TaskHandlerRegistry::new();
        registry
            .register_non_idempotent(
                TaskKind::parse("vendor.commit-race").unwrap(),
                "vendor.commit-race.v1",
                move |_: &TaskHandlerInvocation<'_>| {
                    calls_by_handler.fetch_add(1, Ordering::SeqCst);
                    started_by_handler.store(true, Ordering::Release);
                    let started_at = Instant::now();
                    while !release_by_handler.load(Ordering::Acquire) {
                        assert!(started_at.elapsed() < Duration::from_secs(2));
                        thread::yield_now();
                    }
                    Ok(TaskHandlerResult::new(b"committed-result".as_slice()))
                },
            )
            .unwrap();
        let ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let mut worker = TaskWorkerRuntime::start_paused_with_timing_and_services(
            store.clone(),
            Arc::new(registry),
            Arc::new(InMemoryArtifactStore::new()),
            "daemon:effect:commit-race",
            TaskWorkerTiming::new(Duration::from_millis(5), Duration::from_millis(10)).unwrap(),
            None,
            Some(ledger),
        )
        .unwrap();
        worker.activate();
        wait_until(Duration::from_secs(2), || {
            handler_started.load(Ordering::Acquire)
        });
        let cancelling = store
            .request_cancellation("req-effect-commit-race", "late operator cancellation")
            .unwrap();
        assert!(worker
            .signal_cancellation("req-effect-commit-race", cancelling.cancel_token.as_deref()));
        thread::sleep(Duration::from_millis(550));
        handler_release.store(true, Ordering::Release);

        let completed = wait_for_task_status(&store, "req-effect-commit-race", "completed");
        worker.stop_and_join().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(completed.cancel_requested);
        assert!(!completed.cancel_accepted);
        assert!(completed.deadline_at_ms.unwrap() <= worker_now_ms().unwrap());
        assert_eq!(
            completed.result_digest.as_deref(),
            Some(sha256_digest(b"committed-result").as_str())
        );
        let mut reopened = FileSystemEffectLedger::open_read_only(backend.layout()).unwrap();
        let operation = EffectOperationIdentity::new(
            "idem-effect-commit-race",
            "vendor.commit-race",
            "root-agent",
            "vendor.commit-race.v1",
            envelope.input().digest(),
        )
        .unwrap();
        assert_eq!(
            reopened.inspect(&operation).unwrap().unwrap().state(),
            EffectLedgerState::Committed
        );
    }

    #[test]
    fn legacy_failure_binding_recovers_business_key_and_accepts_existing_envelope() {
        let root = test_root("legacy-failure-binding");
        let backend = eva_storage::FileSystemDurableBackend::open(
            eva_storage::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut store =
            FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer).unwrap();
        let original_task_id = "req-legacy-failure";
        let original_envelope = TaskEnvelope::new(
            TaskKind::parse("runtime.echo").unwrap(),
            AgentId::parse("root-agent").unwrap(),
            TaskInput::inline(b"legacy-replay-payload".as_slice()).unwrap(),
            IdempotencyKey::parse("idem-legacy-business-key").unwrap(),
            TaskAttemptPolicy::new(1, 0, None).unwrap(),
        )
        .unwrap();
        store
            .create(
                &TaskStateSnapshot::queued_with_envelope(
                    original_task_id,
                    original_envelope.to_snapshot(),
                )
                .unwrap(),
            )
            .unwrap();
        let claim = store
            .try_claim_queued(
                original_task_id,
                "daemon:legacy:failure",
                "cancel:legacy:failure",
                100,
            )
            .unwrap()
            .unwrap();
        let failed = store
            .finish_execution(
                claim.fence(),
                &TaskAttemptOutcome::Failed {
                    error_kind: ErrorKind::Unavailable.as_str().to_owned(),
                    error_message: "legacy failure".to_owned(),
                    retryable: true,
                },
            )
            .unwrap();
        let failure_event_id = task_failure_event_id(original_task_id, failed.attempts).unwrap();
        let mut checkpointed = failed;
        checkpointed.dead_letters.push(TaskStateDeadLetterSnapshot {
            event_id: failure_event_id.as_str().to_owned(),
            topic: "/runtime/task/failure".to_owned(),
            reason_kind: ErrorKind::Unavailable.as_str().to_owned(),
            reason: "legacy failure".to_owned(),
            replay_count: 0,
        });
        store.compare_and_set(&checkpointed).unwrap();

        let failure_event = Event::new(
            failure_event_id.clone(),
            Topic::parse("/runtime/task/failure").unwrap(),
            EventPayload::bytes(b"legacy-replay-payload".to_vec()),
        )
        .with_target(EventTarget::Agent(AgentId::parse("root-agent").unwrap()))
        .with_request_id(RequestId::parse(original_task_id).unwrap());
        let replay = failure_event
            .child_event(
                EventId::parse(&format!("{}:replay-1", failure_event_id.as_str())).unwrap(),
                failure_event.topic().clone(),
                failure_event.payload().clone(),
            )
            .with_target(failure_event.target().clone())
            .with_request_id(RequestId::parse(original_task_id).unwrap());
        let legacy_binding =
            ReplayHandlerBinding::new("runtime.echo", AgentId::parse("root-agent").unwrap())
                .unwrap();

        let legacy_task_id = replay_delivery_task_id(&replay, &legacy_binding, 0).unwrap();
        let legacy_envelope = TaskEnvelope::new(
            TaskKind::parse("runtime.echo").unwrap(),
            AgentId::parse("root-agent").unwrap(),
            TaskInput::inline(b"legacy-replay-payload".as_slice()).unwrap(),
            IdempotencyKey::parse(legacy_task_id.as_str()).unwrap(),
            TaskAttemptPolicy::new(u32::MAX, 0, None).unwrap(),
        )
        .unwrap();
        store
            .create(
                &TaskStateSnapshot::queued_with_replay_delivery(
                    legacy_task_id.as_str(),
                    legacy_envelope.to_snapshot(),
                    replay.event_id().as_str(),
                    0,
                )
                .unwrap(),
            )
            .unwrap();

        let mut worker = TaskWorkerRuntime::start(
            store.clone(),
            Arc::new(TaskHandlerRegistry::with_runtime_defaults().unwrap()),
            Arc::new(InMemoryArtifactStore::new()),
            "daemon:legacy:replay",
        )
        .unwrap();
        worker
            .reconcile_replay_delivery(&legacy_binding, &replay, 0, 0, 0)
            .unwrap();
        wait_for_task_status(&store, legacy_task_id.as_str(), "completed");

        let pending = worker
            .reconcile_replay_delivery(&legacy_binding, &replay, 1, 0, 0)
            .unwrap();
        let recovered_task_id = match pending {
            OwnedReplayDeliveryStatus::Pending { task_id, .. } => task_id,
            status => panic!("expected pending recovered replay, got {status:?}"),
        };
        assert_eq!(
            store
                .read(Some(&recovered_task_id))
                .unwrap()
                .envelope
                .unwrap()
                .idempotency_key,
            "idem-legacy-business-key"
        );
        wait_for_task_status(&store, &recovered_task_id, "completed");
        worker.stop_and_join().unwrap();
    }

    #[test]
    fn daemon_owned_worker_executes_bound_replay_with_exact_binary_payload() {
        let root = test_root("owned-replay");
        let backend = eva_storage::FileSystemDurableBackend::open(
            eva_storage::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let observer = store.clone();
        let registry = Arc::new(TaskHandlerRegistry::with_runtime_defaults().unwrap());
        let artifacts: Arc<dyn TaskArtifactResolver> = Arc::new(InMemoryArtifactStore::new());
        let mut worker =
            TaskWorkerRuntime::start(store, registry, artifacts, "replay-owner-1").unwrap();
        let payload = vec![0, 0xff, b'\n', b'=', 0x80];
        let replay = Event::new(
            EventId::parse("evt-owned-replay").unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::bytes(payload.clone()),
        )
        .with_request_id(RequestId::parse("req-owned-replay").unwrap());
        let binding =
            ReplayHandlerBinding::new("runtime.echo", AgentId::parse("root-agent").unwrap())
                .unwrap()
                .with_idempotency_key(RequestId::parse("idem-owned-replay").unwrap());

        let pending = worker
            .reconcile_replay_delivery(&binding, &replay, 0, 0, 0)
            .unwrap();
        let task_id = match pending {
            OwnedReplayDeliveryStatus::Pending { task_id, .. } => task_id,
            status => panic!("expected pending replay delivery, got {status:?}"),
        };
        assert_eq!(
            observer
                .read(Some(&task_id))
                .unwrap()
                .envelope
                .unwrap()
                .idempotency_key,
            "idem-owned-replay"
        );
        wait_for_task_status(&observer, &task_id, "completed");
        let completed = worker
            .reconcile_replay_delivery(&binding, &replay, 0, 0, 0)
            .unwrap();
        match completed {
            OwnedReplayDeliveryStatus::Succeeded {
                result_digest,
                result_size_bytes,
                ..
            } => {
                assert_eq!(result_digest, sha256_digest(&payload));
                assert_eq!(result_size_bytes, payload.len());
            }
            status => panic!("expected successful replay delivery, got {status:?}"),
        }
        worker.stop_and_join().unwrap();
    }

    #[test]
    fn daemon_owned_worker_persists_unknown_replay_handler_failure() {
        let root = test_root("owned-replay-errors");
        let backend = eva_storage::FileSystemDurableBackend::open(
            eva_storage::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let observer = store.clone();
        let registry = Arc::new(TaskHandlerRegistry::with_runtime_defaults().unwrap());
        let artifacts: Arc<dyn TaskArtifactResolver> = Arc::new(InMemoryArtifactStore::new());
        let mut worker =
            TaskWorkerRuntime::start(store, registry, artifacts, "replay-owner-2").unwrap();
        let event = Event::new(
            EventId::parse("evt-replay-no-request").unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::empty(),
        );
        let unknown_binding =
            ReplayHandlerBinding::new("vendor.unknown", AgentId::parse("root-agent").unwrap())
                .unwrap();
        let pending = worker
            .reconcile_replay_delivery(&unknown_binding, &event, 0, 0, 0)
            .unwrap();
        let task_id = match pending {
            OwnedReplayDeliveryStatus::Pending { task_id, .. } => task_id,
            status => panic!("expected pending replay delivery, got {status:?}"),
        };
        thread::sleep(Duration::from_millis(100));
        worker.check_health().unwrap();
        wait_for_task_status(&observer, &task_id, "failed");
        let failed = worker
            .reconcile_replay_delivery(&unknown_binding, &event, 0, 0, 0)
            .unwrap();
        match failed {
            OwnedReplayDeliveryStatus::Failed {
                error,
                retry_scheduled,
                ..
            } => {
                assert_eq!(error.kind(), ErrorKind::NotFound);
                assert_eq!(error.message(), TASK_HANDLER_NOT_REGISTERED_MESSAGE);
                assert!(!retry_scheduled);
            }
            status => panic!("expected failed replay delivery, got {status:?}"),
        }
        worker.stop_and_join().unwrap();
    }

    #[test]
    fn retryable_replay_delivery_requeues_and_completes_under_worker_fence() {
        let root = test_root("owned-replay-retry");
        let backend = eva_storage::FileSystemDurableBackend::open(
            eva_storage::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let observer = store.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = Arc::clone(&calls);
        let mut registry = TaskHandlerRegistry::new();
        registry
            .register(
                TaskKind::parse("vendor.retry-once").unwrap(),
                move |invocation: &TaskHandlerInvocation<'_>| {
                    if handler_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err(EvaError::timeout("retry once"))
                    } else {
                        Ok(TaskHandlerResult::new(invocation.payload()))
                    }
                },
            )
            .unwrap();
        let artifacts: Arc<dyn TaskArtifactResolver> = Arc::new(InMemoryArtifactStore::new());
        let mut worker =
            TaskWorkerRuntime::start(store, Arc::new(registry), artifacts, "replay-owner-retry")
                .unwrap();
        let replay = Event::new(
            EventId::parse("evt-owned-replay-retry").unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::text("retry-payload"),
        );
        let binding =
            ReplayHandlerBinding::new("vendor.retry-once", AgentId::parse("root-agent").unwrap())
                .unwrap();
        let pending = worker
            .reconcile_replay_delivery(&binding, &replay, 0, 0, 0)
            .unwrap();
        let task_id = match pending {
            OwnedReplayDeliveryStatus::Pending { task_id, .. } => task_id,
            status => panic!("expected pending replay delivery, got {status:?}"),
        };
        wait_for_task_status(&observer, &task_id, "timed_out");

        let failed = worker
            .reconcile_replay_delivery(&binding, &replay, 0, 0, 0)
            .unwrap();
        assert!(matches!(
            failed,
            OwnedReplayDeliveryStatus::Failed {
                retry_scheduled: true,
                ..
            }
        ));
        wait_for_task_status(&observer, &task_id, "completed");
        assert!(matches!(
            worker
                .reconcile_replay_delivery(&binding, &replay, 0, 0, 0)
                .unwrap(),
            OwnedReplayDeliveryStatus::Succeeded { .. }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        worker.stop_and_join().unwrap();
    }

    #[test]
    fn replay_retry_respects_explicit_non_retryable_timeout_override() {
        let root = test_root("owned-replay-non-retryable");
        let backend = eva_storage::FileSystemDurableBackend::open(
            eva_storage::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let store = FileSystemTaskStateStore::from_writable_backend(&backend).unwrap();
        let observer = store.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = Arc::clone(&calls);
        let mut registry = TaskHandlerRegistry::new();
        registry
            .register(
                TaskKind::parse("vendor.non-retryable-timeout").unwrap(),
                move |_: &TaskHandlerInvocation<'_>| {
                    handler_calls.fetch_add(1, Ordering::SeqCst);
                    Err(EvaError::timeout("permanent timeout classification").with_retryable(false))
                },
            )
            .unwrap();
        let artifacts: Arc<dyn TaskArtifactResolver> = Arc::new(InMemoryArtifactStore::new());
        let mut worker = TaskWorkerRuntime::start(
            store,
            Arc::new(registry),
            artifacts,
            "replay-owner-non-retryable",
        )
        .unwrap();
        let replay = Event::new(
            EventId::parse("evt-owned-replay-non-retryable").unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::text("payload"),
        );
        let binding = ReplayHandlerBinding::new(
            "vendor.non-retryable-timeout",
            AgentId::parse("root-agent").unwrap(),
        )
        .unwrap();
        let pending = worker
            .reconcile_replay_delivery(&binding, &replay, 0, 1_000, 100)
            .unwrap();
        let task_id = match pending {
            OwnedReplayDeliveryStatus::Pending { task_id, .. } => task_id,
            status => panic!("expected pending replay delivery, got {status:?}"),
        };
        wait_for_task_status(&observer, &task_id, "timed_out");

        match worker
            .reconcile_replay_delivery(&binding, &replay, 0, 1_000, 10_000)
            .unwrap()
        {
            OwnedReplayDeliveryStatus::Failed {
                error,
                retry_scheduled,
                ..
            } => {
                assert_eq!(error.kind(), ErrorKind::Timeout);
                assert!(!error.is_retryable());
                assert!(!retry_scheduled);
            }
            status => panic!("expected failed replay delivery, got {status:?}"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let persisted = observer.read(Some(&task_id)).unwrap();
        assert_eq!(persisted.error_retryable, Some(false));
        assert!(persisted.retry_ready_at_ms.is_none());
        worker.stop_and_join().unwrap();
    }

    #[test]
    fn production_task_failure_persists_executable_replay_binding() {
        let root = test_root("task-failure-binding");
        let backend = eva_storage::FileSystemDurableBackend::open(
            eva_storage::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut store =
            FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
                .unwrap();
        let observer = store.clone();
        let task = TaskEnvelope::new(
            TaskKind::parse("vendor.production-missing").unwrap(),
            AgentId::parse("root-agent").unwrap(),
            TaskInput::inline(b"durable-failure-payload".as_slice()).unwrap(),
            IdempotencyKey::parse("idem-production-failure").unwrap(),
            TaskAttemptPolicy::new(1, 5_000, None).unwrap(),
        )
        .unwrap();
        store
            .create(
                &TaskStateSnapshot::queued_with_envelope(
                    "req-production-failure",
                    task.to_snapshot(),
                )
                .unwrap(),
            )
            .unwrap();
        let failure_bus = DurableEventBus::open_with_writer(backend.layout(), writer).unwrap();
        let registry = Arc::new(TaskHandlerRegistry::with_runtime_defaults().unwrap());
        let artifacts: Arc<dyn TaskArtifactResolver> = Arc::new(InMemoryArtifactStore::new());
        let mut worker = TaskWorkerRuntime::start_paused_with_failure_bus(
            store,
            registry,
            artifacts,
            "production-failure-owner",
            failure_bus,
        )
        .unwrap();
        let retry_clock_floor = u64::try_from(worker_now_ms().unwrap()).unwrap();
        worker.activate();

        thread::sleep(Duration::from_millis(100));
        worker.check_health().unwrap();
        wait_for_task_status(&observer, "req-production-failure", "failed");
        worker.stop_and_join().unwrap();

        let bus = DurableEventBus::open_read_only(backend.layout()).unwrap();
        assert_eq!(bus.dead_letters().len(), 1);
        let dead_letter = &bus.dead_letters()[0];
        assert_eq!(dead_letter.replay_handlers.len(), 1);
        assert_eq!(dead_letter.redrive.retry_delay_ms, 5_000);
        assert!(
            dead_letter.redrive.next_attempt_after_ms >= retry_clock_floor.saturating_add(5_000)
        );
        assert_eq!(
            dead_letter.replay_handlers[0].handler_kind(),
            "vendor.production-missing"
        );
        assert_eq!(
            dead_letter.replay_handlers[0].agent_id().as_str(),
            "root-agent"
        );
        assert_eq!(
            dead_letter.replay_handlers[0]
                .idempotency_key()
                .map(RequestId::as_str),
            Some("idem-production-failure")
        );
        assert_eq!(
            dead_letter
                .event
                .metadata()
                .request_id()
                .map(RequestId::as_str),
            Some("req-production-failure")
        );
        assert_eq!(
            dead_letter.event.payload().as_bytes(),
            Some(b"durable-failure-payload".as_slice())
        );
        assert_eq!(
            bus.event_log_status(dead_letter.event_id()),
            Some(eva_storage::EventLogStatus::Appended)
        );
        let task = observer.read(Some("req-production-failure")).unwrap();
        assert_eq!(task.dead_letters.len(), 1);
        assert_eq!(
            task.dead_letters[0].event_id,
            dead_letter.event_id().as_str()
        );
    }

    #[test]
    fn worker_rebuilds_dead_letter_from_terminal_intent_after_event_only_crash() {
        let root = test_root("task-failure-event-only-crash");
        let task_id = "req-production-failure-recovery";
        let failure_event_id = {
            let backend = eva_storage::FileSystemDurableBackend::open(
                eva_storage::DurableBackendOptions::read_write(root.path()),
            )
            .unwrap();
            let writer = backend.acquire_runtime_writer().unwrap();
            let mut store =
                FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
                    .unwrap();
            let task = TaskEnvelope::new(
                TaskKind::parse("vendor.failure-recovery").unwrap(),
                AgentId::parse("root-agent").unwrap(),
                TaskInput::inline(b"event-only-payload".as_slice()).unwrap(),
                IdempotencyKey::parse("idem-production-failure-recovery").unwrap(),
                TaskAttemptPolicy::new(2, 250, None).unwrap(),
            )
            .unwrap();
            store
                .create(
                    &TaskStateSnapshot::queued_with_envelope(task_id, task.to_snapshot()).unwrap(),
                )
                .unwrap();
            let claim = store
                .try_claim_queued(task_id, "daemon.failure.old", "cancel.failure.old", 100)
                .unwrap()
                .unwrap();
            let failed = store
                .finish_execution(
                    claim.fence(),
                    &TaskAttemptOutcome::Failed {
                        error_kind: ErrorKind::Unavailable.as_str().to_owned(),
                        error_message: "provider temporarily unavailable".to_owned(),
                        retryable: true,
                    },
                )
                .unwrap();
            assert_eq!(failed.status, "failed");
            assert_eq!(failed.error_retryable, Some(true));
            assert!(failed.dead_letters.is_empty());

            let event_id = task_failure_event_id(task_id, failed.attempts).unwrap();
            let event = Event::new(
                event_id.clone(),
                Topic::parse("/runtime/task/failure").unwrap(),
                EventPayload::bytes(b"event-only-payload".to_vec()),
            )
            .with_target(EventTarget::Agent(AgentId::parse("root-agent").unwrap()))
            .with_request_id(RequestId::parse(task_id).unwrap());
            let mut bus = DurableEventBus::open_with_writer(backend.layout(), writer).unwrap();
            bus.publish(event).unwrap();
            assert!(bus.dead_letters().is_empty());
            event_id
        };

        let backend = eva_storage::FileSystemDurableBackend::open(
            eva_storage::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let store = FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
            .unwrap();
        let observer = store.clone();
        let failure_bus = DurableEventBus::open_with_writer(backend.layout(), writer).unwrap();
        let registry = Arc::new(TaskHandlerRegistry::with_runtime_defaults().unwrap());
        let artifacts: Arc<dyn TaskArtifactResolver> = Arc::new(InMemoryArtifactStore::new());
        let mut worker = TaskWorkerRuntime::start_paused_with_failure_bus(
            store,
            registry,
            artifacts,
            "production-failure-recovery-owner",
            failure_bus,
        )
        .unwrap();
        worker.activate();

        let started = std::time::Instant::now();
        loop {
            let snapshot = observer.read(Some(task_id)).unwrap();
            if snapshot
                .dead_letters
                .iter()
                .any(|record| record.event_id == failure_event_id.as_str())
            {
                break;
            }
            worker.check_health().unwrap();
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "worker did not rebuild task failure dead-letter evidence"
            );
            thread::sleep(Duration::from_millis(10));
        }
        worker.stop_and_join().unwrap();

        let bus = DurableEventBus::open_read_only(backend.layout()).unwrap();
        assert_eq!(
            bus.log()
                .records()
                .iter()
                .filter(|record| record.event.event_id() == &failure_event_id)
                .count(),
            1
        );
        let dead_letter = bus
            .dead_letters()
            .iter()
            .find(|record| record.event_id() == &failure_event_id)
            .unwrap();
        assert_eq!(dead_letter.replay_handlers.len(), 1);
        assert_eq!(
            dead_letter.replay_handlers[0].handler_kind(),
            "vendor.failure-recovery"
        );
        assert_eq!(dead_letter.redrive.retry_delay_ms, 250);
    }

    #[test]
    fn worker_checkpoints_existing_legacy_dead_letter_without_business_key() {
        let root = test_root("legacy-dead-letter-checkpoint");
        let task_id = "req-legacy-dead-letter-checkpoint";
        let backend = eva_storage::FileSystemDurableBackend::open(
            eva_storage::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut store =
            FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
                .unwrap();
        let envelope = TaskEnvelope::new(
            TaskKind::parse("vendor.legacy-dead-letter").unwrap(),
            AgentId::parse("root-agent").unwrap(),
            TaskInput::inline(b"legacy-dead-letter-payload".as_slice()).unwrap(),
            IdempotencyKey::parse("idem-legacy-dead-letter").unwrap(),
            TaskAttemptPolicy::new(2, 250, None).unwrap(),
        )
        .unwrap();
        store
            .create(
                &TaskStateSnapshot::queued_with_envelope(task_id, envelope.to_snapshot()).unwrap(),
            )
            .unwrap();
        let claim = store
            .try_claim_queued(
                task_id,
                "daemon:legacy:dead-letter",
                "cancel:legacy:dead-letter",
                100,
            )
            .unwrap()
            .unwrap();
        let failed = store
            .finish_execution(
                claim.fence(),
                &TaskAttemptOutcome::Failed {
                    error_kind: ErrorKind::Unavailable.as_str().to_owned(),
                    error_message: "legacy crash window".to_owned(),
                    retryable: true,
                },
            )
            .unwrap();
        assert!(failed.dead_letters.is_empty());
        let event_id = task_failure_event_id(task_id, failed.attempts).unwrap();
        let event = Event::new(
            event_id.clone(),
            Topic::parse("/runtime/task/failure").unwrap(),
            EventPayload::bytes(b"legacy-dead-letter-payload".to_vec()),
        )
        .with_target(EventTarget::Agent(AgentId::parse("root-agent").unwrap()))
        .with_request_id(RequestId::parse(task_id).unwrap());
        let mut failure_bus =
            DurableEventBus::open_with_writer(backend.layout(), writer.clone()).unwrap();
        failure_bus.publish(event.clone()).unwrap();
        failure_bus
            .dead_letter_for_handlers_with_redrive(
                event,
                EvaError::unavailable("legacy crash window"),
                vec![ReplayHandlerBinding::new(
                    "vendor.legacy-dead-letter",
                    AgentId::parse("root-agent").unwrap(),
                )
                .unwrap()],
                RedrivePolicy {
                    retry_delay_ms: 250,
                    next_attempt_after_ms: 1,
                },
            )
            .unwrap();

        let mut worker = TaskWorkerRuntime::start_paused_with_failure_bus(
            store.clone(),
            Arc::new(TaskHandlerRegistry::with_runtime_defaults().unwrap()),
            Arc::new(InMemoryArtifactStore::new()),
            "daemon:legacy:checkpoint",
            failure_bus,
        )
        .unwrap();
        worker.activate();
        let started = Instant::now();
        let checkpointed = loop {
            let snapshot = store.read(Some(task_id)).unwrap();
            if snapshot.dead_letters.len() == 1 {
                break snapshot;
            }
            worker.check_health().unwrap();
            assert!(started.elapsed() < Duration::from_secs(2));
            thread::sleep(Duration::from_millis(5));
        };
        worker.stop_and_join().unwrap();

        assert_eq!(checkpointed.dead_letters[0].event_id, event_id.as_str());
        let bus = DurableEventBus::open_read_only(backend.layout()).unwrap();
        let record = bus
            .dead_letters()
            .iter()
            .find(|record| record.event_id() == &event_id)
            .unwrap();
        assert_eq!(record.replay_handlers.len(), 1);
        assert!(record.replay_handlers[0].idempotency_key().is_none());
    }

    #[test]
    fn stale_workers_reconcile_one_terminal_failure_evidence_without_fatal_conflict() {
        let root = test_root("two-workers-one-failure-evidence");
        let backend = eva_storage::FileSystemDurableBackend::open(
            eva_storage::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut store =
            FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
                .unwrap();
        let task_id = "req-two-workers-one-failure";
        let task = TaskEnvelope::new(
            TaskKind::parse("vendor.concurrent-failure").unwrap(),
            AgentId::parse("root-agent").unwrap(),
            TaskInput::inline(b"concurrent-failure-payload".as_slice()).unwrap(),
            IdempotencyKey::parse("idem-two-workers-one-failure").unwrap(),
            TaskAttemptPolicy::new(2, 250, None).unwrap(),
        )
        .unwrap();
        store
            .create(&TaskStateSnapshot::queued_with_envelope(task_id, task.to_snapshot()).unwrap())
            .unwrap();
        let claim = store
            .try_claim_queued(
                task_id,
                "daemon.concurrent.old",
                "cancel.concurrent.old",
                100,
            )
            .unwrap()
            .unwrap();
        let failed = store
            .finish_execution(
                claim.fence(),
                &TaskAttemptOutcome::Failed {
                    error_kind: ErrorKind::Unavailable.as_str().to_owned(),
                    error_message: "provider temporarily unavailable".to_owned(),
                    retryable: true,
                },
            )
            .unwrap();
        assert!(failed.dead_letters.is_empty());

        // Both worker buses intentionally retain a view from before the competing event/dead-letter
        // commit. Their reconciliation must refresh and validate that commit instead of exiting.
        let first_failure_bus =
            DurableEventBus::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let second_failure_bus =
            DurableEventBus::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let artifacts: Arc<dyn TaskArtifactResolver> = Arc::new(InMemoryArtifactStore::new());
        let event_id = task_failure_event_id(task_id, failed.attempts).unwrap();
        let mut seed_bus =
            DurableEventBus::open_with_writer(backend.layout(), writer.clone()).unwrap();
        record_task_failure_dead_letter(
            &mut seed_bus,
            artifacts.as_ref(),
            &failed,
            event_id.clone(),
        )
        .unwrap();
        drop(seed_bus);

        let registry = Arc::new(TaskHandlerRegistry::with_runtime_defaults().unwrap());
        let mut first = TaskWorkerRuntime::start_paused_with_failure_bus(
            store.clone(),
            Arc::clone(&registry),
            Arc::clone(&artifacts),
            "daemon:g1:failure-worker-1",
            first_failure_bus,
        )
        .unwrap();
        let mut second = TaskWorkerRuntime::start_paused_with_failure_bus(
            store.clone(),
            registry,
            artifacts,
            "daemon:g1:failure-worker-2",
            second_failure_bus,
        )
        .unwrap();
        first.activate();
        second.activate();

        let started = Instant::now();
        let checkpointed = loop {
            let snapshot = store.read(Some(task_id)).unwrap();
            if snapshot.dead_letters.len() == 1 {
                break snapshot;
            }
            first.check_health().unwrap();
            second.check_health().unwrap();
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "workers did not reconcile the terminal failure evidence"
            );
            thread::sleep(Duration::from_millis(10));
        };
        first.check_health().unwrap();
        second.check_health().unwrap();
        first.stop_and_join().unwrap();
        second.stop_and_join().unwrap();

        assert_eq!(checkpointed.dead_letters[0].event_id, event_id.as_str());
        let bus = DurableEventBus::open_read_only(backend.layout()).unwrap();
        assert_eq!(
            bus.log()
                .records()
                .iter()
                .filter(|record| record.event.event_id() == &event_id)
                .count(),
            1
        );
        assert_eq!(
            bus.dead_letters()
                .iter()
                .filter(|record| record.event_id() == &event_id)
                .count(),
            1
        );
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

        thread::sleep(Duration::from_millis(100));
        first.check_health().unwrap();
        second.check_health().unwrap();
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
    fn active_worker_renews_heartbeat_and_cannot_be_reclaimed() {
        let root = test_root("worker-heartbeat-owner");
        let mut store = FileSystemTaskStateStore::new(root.path());
        let task = envelope(
            "vendor.heartbeat",
            TaskInput::inline(b"keep-alive".as_slice()).unwrap(),
        );
        store
            .create(
                &eva_storage::TaskStateSnapshot::queued_with_envelope(
                    "req-worker-heartbeat",
                    task.to_snapshot(),
                )
                .unwrap(),
            )
            .unwrap();
        let sentinel = envelope(
            "runtime.echo",
            TaskInput::inline(b"competitor-scanned".as_slice()).unwrap(),
        );
        store
            .create(
                &eva_storage::TaskStateSnapshot::queued_with_envelope(
                    "req-worker-z-heartbeat-sentinel",
                    sentinel.to_snapshot(),
                )
                .unwrap(),
            )
            .unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_by_handler = Arc::clone(&calls);
        let handler_started = Arc::new(AtomicBool::new(false));
        let started_by_handler = Arc::clone(&handler_started);
        let release_handler = Arc::new(AtomicBool::new(false));
        let release_by_handler = Arc::clone(&release_handler);
        let mut registry = TaskHandlerRegistry::with_runtime_defaults().unwrap();
        registry
            .register(
                TaskKind::parse("vendor.heartbeat").unwrap(),
                move |invocation: &TaskHandlerInvocation<'_>| {
                    calls_by_handler.fetch_add(1, Ordering::SeqCst);
                    started_by_handler.store(true, Ordering::Release);
                    while !release_by_handler.load(Ordering::Acquire) {
                        assert!(!invocation.cancellation().is_requested());
                        thread::yield_now();
                    }
                    Ok(TaskHandlerResult::new(invocation.payload()))
                },
            )
            .unwrap();
        let registry = Arc::new(registry);
        let artifacts: Arc<dyn TaskArtifactResolver> = Arc::new(InMemoryArtifactStore::new());
        let timing =
            TaskWorkerTiming::new(Duration::from_millis(5), Duration::from_millis(10)).unwrap();
        let mut owner = TaskWorkerRuntime::start_paused_with_timing(
            store.clone(),
            Arc::clone(&registry),
            Arc::clone(&artifacts),
            "daemon:g1:heartbeat-owner",
            timing,
        )
        .unwrap();
        owner.activate();
        wait_until(Duration::from_secs(2), || {
            handler_started.load(Ordering::Acquire)
        });
        let initial_heartbeat = store
            .read(Some("req-worker-heartbeat"))
            .unwrap()
            .heartbeat_at_ms
            .unwrap();
        wait_until(Duration::from_secs(2), || {
            store
                .read(Some("req-worker-heartbeat"))
                .unwrap()
                .heartbeat_at_ms
                .is_some_and(|heartbeat| heartbeat > initial_heartbeat)
        });

        let mut competitor = TaskWorkerRuntime::start_paused_with_timing(
            store.clone(),
            registry,
            artifacts,
            "daemon:g1:heartbeat-competitor",
            timing,
        )
        .unwrap();
        competitor.activate();
        competitor.notify_new_work();
        let sentinel = wait_for_task_status(&store, "req-worker-z-heartbeat-sentinel", "completed");

        let active = store.read(Some("req-worker-heartbeat")).unwrap();
        assert_eq!(active.status, "running");
        assert_eq!(active.attempts, 1);
        assert_eq!(
            active.execution_owner.as_deref(),
            Some("daemon:g1:heartbeat-owner")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            sentinel.execution_owner.as_deref(),
            Some("daemon:g1:heartbeat-competitor")
        );
        assert_eq!(
            active.default_freshness_at(worker_now_ms().unwrap()),
            eva_storage::TaskFreshness::Live
        );

        release_handler.store(true, Ordering::Release);
        let completed = wait_for_task_status(&store, "req-worker-heartbeat", "completed");
        owner.stop_and_join().unwrap();
        competitor.stop_and_join().unwrap();
        assert_eq!(completed.attempts, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
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

    struct BlockingResolver {
        record: ArtifactRecord,
        started: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
    }

    impl TaskArtifactResolver for BlockingResolver {
        fn resolve_task_artifact(
            &self,
            _reference: &TaskArtifactRef,
        ) -> Result<Option<ArtifactRecord>, EvaError> {
            self.started.store(true, Ordering::Release);
            let started_at = Instant::now();
            while !self.release.load(Ordering::Acquire) {
                assert!(
                    started_at.elapsed() < Duration::from_secs(2),
                    "artifact resolver was not released"
                );
                thread::yield_now();
            }
            Ok(Some(self.record.clone()))
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
