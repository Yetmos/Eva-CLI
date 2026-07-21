//! Provider credential vault and per-invocation session boundaries.
//!
//! A manifest carries references only.  A transport receives bytes through a
//! short-lived [`CredentialSessionLease`], and the lease clears its in-memory
//! projection when the invocation finishes.  The production default is
//! deliberately fail-closed; callers must wire an OS/KMS-backed implementation
//! before a provider that declares credentials can run.

use crate::supervisor::{redact_provider_session_tokens, ProviderCredentialScope};
use eva_config::ProviderVaultSecretRef;
use eva_core::EvaError;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::env;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};
use std::thread;
use std::time::Duration;

/// A vault implementation opens one session for one admitted provider scope.
pub trait CredentialVault: fmt::Debug + Send + Sync {
    /// Open a short-lived session.  Implementations must not put secret bytes
    /// in returned errors or debug output.
    fn open_session(
        &self,
        scope: &ProviderCredentialScope,
    ) -> Result<Box<dyn CredentialSession>, EvaError>;
}

/// Session-level secret access.  Implementations may fetch lazily, but must
/// reject every operation after [`CredentialSession::release`] succeeds.
pub trait CredentialSession: fmt::Debug + Send {
    /// Fetch one opaque vault reference.
    fn fetch(&mut self, secret_ref: &str) -> Result<SecretValue, EvaError>;
    /// Revoke/close the session and release provider-side material.
    fn release(&mut self) -> Result<(), EvaError>;
}

/// Cloneable runtime handle around a vault trait object.  Equality is pointer
/// identity so existing `AdapterRuntime` value semantics remain available
/// without ever comparing or formatting secret material.
#[derive(Clone)]
pub struct CredentialVaultHandle(Arc<dyn CredentialVault>);

impl CredentialVaultHandle {
    /// Wrap one vault implementation for runtime ownership.
    pub fn new(vault: impl CredentialVault + 'static) -> Self {
        Self(Arc::new(vault))
    }

    /// Wrap an already shared implementation.
    pub fn from_shared(vault: Arc<dyn CredentialVault>) -> Self {
        Self(vault)
    }

    /// Borrow the underlying authority.
    pub(crate) fn as_ref(&self) -> &dyn CredentialVault {
        self.0.as_ref()
    }
}

impl fmt::Debug for CredentialVaultHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialVaultHandle([REDACTED_AUTHORITY])")
    }
}

impl PartialEq for CredentialVaultHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for CredentialVaultHandle {}

/// Secret bytes wrapped in a type whose formatting is always redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(String);

impl SecretValue {
    /// Construct a secret returned by an explicit vault implementation.
    /// Formatting this value is always redacted; callers should only expose
    /// it to the transport that owns the current credential lease.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        wipe_string(&mut self.0);
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

/// Production-safe default.  It does not read the daemon environment or any
/// local file, so accidentally omitting the real vault wiring fails closed.
#[derive(Debug, Clone, Copy, Default)]
pub struct FailClosedCredentialVault;

impl CredentialVault for FailClosedCredentialVault {
    fn open_session(
        &self,
        _scope: &ProviderCredentialScope,
    ) -> Result<Box<dyn CredentialSession>, EvaError> {
        Err(
            EvaError::permission_denied("provider credential vault is not configured")
                .with_provider_code("credential_vault_unconfigured"),
        )
    }
}

/// In-memory vault intended for explicit tests and controlled local fixtures.
/// It is never selected by production constructors implicitly.
#[derive(Clone, Default)]
pub struct MemoryCredentialVault {
    secrets: Arc<RwLock<BTreeMap<String, String>>>,
}

impl fmt::Debug for MemoryCredentialVault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.secrets.read().map(|map| map.len()).unwrap_or(0);
        formatter
            .debug_struct("MemoryCredentialVault")
            .field("secret_count", &count)
            .finish()
    }
}

impl MemoryCredentialVault {
    /// Create an empty memory vault.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace one reference without exposing it through `Debug`.
    pub fn insert(&self, secret_ref: impl Into<String>, value: impl Into<String>) {
        if let Ok(mut secrets) = self.secrets.write() {
            secrets.insert(secret_ref.into(), value.into());
        }
    }

    /// Builder form useful in tests.
    pub fn with_secret(self, secret_ref: impl Into<String>, value: impl Into<String>) -> Self {
        self.insert(secret_ref, value);
        self
    }
}

impl CredentialVault for MemoryCredentialVault {
    fn open_session(
        &self,
        scope: &ProviderCredentialScope,
    ) -> Result<Box<dyn CredentialSession>, EvaError> {
        let secrets = self
            .secrets
            .read()
            .map_err(|_| EvaError::internal("credential vault lock is poisoned"))?
            .clone();
        Ok(Box::new(MemoryCredentialSession {
            scope_id: scope.session_id.clone(),
            secrets,
            released: false,
            fetched: Vec::new(),
        }))
    }
}

#[derive(Clone)]
struct MemoryCredentialSession {
    scope_id: String,
    secrets: BTreeMap<String, String>,
    released: bool,
    fetched: Vec<String>,
}

impl fmt::Debug for MemoryCredentialSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryCredentialSession")
            .field("scope_id", &self.scope_id)
            .field("secret_count", &self.secrets.len())
            .field("released", &self.released)
            .finish()
    }
}

impl CredentialSession for MemoryCredentialSession {
    fn fetch(&mut self, secret_ref: &str) -> Result<SecretValue, EvaError> {
        if self.released {
            return Err(EvaError::conflict(
                "provider credential session has been released",
            ));
        }
        let lookup = secret_ref
            .strip_prefix("env:")
            .unwrap_or(secret_ref)
            .to_owned();
        let value = self
            .secrets
            .get(secret_ref)
            .or_else(|| self.secrets.get(&lookup))
            .ok_or_else(|| {
                EvaError::unavailable("provider credential reference is unavailable")
                    .with_provider_code("missing_credential")
                    .with_context("secret_ref", secret_ref)
            })?;
        self.fetched.push(secret_ref.to_owned());
        Ok(SecretValue::new(value.clone()))
    }

    fn release(&mut self) -> Result<(), EvaError> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        wipe_map_values(&mut self.secrets);
        self.fetched.clear();
        Ok(())
    }
}

/// A transport-facing projection of one vault session.
pub struct CredentialSessionLease {
    session: Option<Box<dyn CredentialSession>>,
    values: BTreeMap<String, String>,
    allowed_envs: BTreeSet<String>,
    redactions: Vec<String>,
    audit: Vec<String>,
    released: bool,
    finalizer_scope: u64,
}

struct DeferredCredentialSession {
    session: Box<dyn CredentialSession>,
    scope: u64,
}

struct PendingCredentialRelease {
    owner: DeferredCredentialSession,
    last_attempt_epoch: Option<u64>,
}

struct RunningCredentialRelease {
    worker: thread::JoinHandle<CredentialReleaseOutcome>,
    attempt_epoch: u64,
    scope: u64,
}

enum CredentialReleaseOutcome {
    Released,
    Failed(DeferredCredentialSession),
}

enum CredentialFinalizerCommand {
    Enqueue(DeferredCredentialSession),
    Retry {
        scope: u64,
        deadline: std::time::Instant,
        acknowledgement: mpsc::SyncSender<()>,
    },
}

#[derive(Debug, Clone, Copy, Default)]
struct CredentialFinalizerProgress {
    outstanding: usize,
    pending: usize,
    running: usize,
    quarantined: usize,
}

struct CredentialFinalizerShared {
    progress: Mutex<BTreeMap<u64, CredentialFinalizerProgress>>,
    changed: Condvar,
}

struct CredentialReleaseFinalizer {
    sender: mpsc::Sender<CredentialFinalizerCommand>,
    shared: Arc<CredentialFinalizerShared>,
    _worker: thread::JoinHandle<()>,
}

pub(crate) struct CredentialFinalizerRunningGuard {
    shared: Arc<CredentialFinalizerShared>,
    scope: u64,
}

static CREDENTIAL_RELEASE_FINALIZER: OnceLock<CredentialReleaseFinalizer> = OnceLock::new();
const CREDENTIAL_RELEASE_FINALIZER_POLL_INTERVAL: Duration = Duration::from_millis(10);
const CREDENTIAL_RELEASE_FINALIZER_MAX_WORKERS: usize = 4;

#[cfg(test)]
thread_local! {
    static CREDENTIAL_FINALIZER_TEST_SCOPE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
static NEXT_CREDENTIAL_FINALIZER_TEST_SCOPE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

#[cfg(test)]
pub(crate) struct CredentialFinalizerTestGuard {
    previous_scope: u64,
}

fn credential_release_finalizer() -> Result<&'static CredentialReleaseFinalizer, EvaError> {
    if let Some(finalizer) = CREDENTIAL_RELEASE_FINALIZER.get() {
        return Ok(finalizer);
    }

    let (sender, receiver) = mpsc::channel();
    let shared = Arc::new(CredentialFinalizerShared {
        progress: Mutex::new(BTreeMap::new()),
        changed: Condvar::new(),
    });
    let worker_shared = Arc::clone(&shared);
    let worker = thread::Builder::new()
        .name("eva-credential-finalizer".to_owned())
        .spawn(move || run_credential_release_finalizer(receiver, worker_shared))
        .map_err(|error| {
            EvaError::unavailable("provider credential finalizer could not start")
                .with_provider_code("credential_finalizer_spawn_failed")
                .with_context("os_error_kind", format!("{:?}", error.kind()))
        })?;
    let candidate = CredentialReleaseFinalizer {
        sender,
        shared,
        _worker: worker,
    };
    if let Err(candidate) = CREDENTIAL_RELEASE_FINALIZER.set(candidate) {
        drop(candidate.sender);
        let _ = candidate._worker.join();
    }
    CREDENTIAL_RELEASE_FINALIZER.get().ok_or_else(|| {
        EvaError::internal("provider credential finalizer initialization was lost")
            .with_provider_code("credential_finalizer_missing")
    })
}

fn run_credential_release_finalizer(
    receiver: Receiver<CredentialFinalizerCommand>,
    shared: Arc<CredentialFinalizerShared>,
) {
    let mut pending = VecDeque::new();
    let mut workers = Vec::new();
    let mut disconnected = false;
    let mut retry_authorizations = BTreeMap::new();

    while !disconnected || !pending.is_empty() || !workers.is_empty() {
        let mut acknowledgements = Vec::new();
        if !disconnected {
            match receiver.recv_timeout(CREDENTIAL_RELEASE_FINALIZER_POLL_INTERVAL) {
                Ok(command) => apply_credential_finalizer_command(
                    command,
                    &mut pending,
                    &mut retry_authorizations,
                    &mut acknowledgements,
                ),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => disconnected = true,
            }
            loop {
                match receiver.try_recv() {
                    Ok(command) => apply_credential_finalizer_command(
                        command,
                        &mut pending,
                        &mut retry_authorizations,
                        &mut acknowledgements,
                    ),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        } else if !pending.is_empty() || !workers.is_empty() {
            thread::sleep(CREDENTIAL_RELEASE_FINALIZER_POLL_INTERVAL);
        }

        reap_credential_release_workers(&mut workers, &mut pending, &shared);

        let pending_count = pending.len();
        for _ in 0..pending_count {
            let mut task = pending
                .pop_front()
                .expect("credential finalizer pending count remains stable");
            let (retry_epoch, retry_deadline) = retry_authorizations
                .get(&task.owner.scope)
                .copied()
                .unwrap_or((0, None));
            let retry_is_authorized = task.last_attempt_epoch.is_none_or(|attempted| {
                attempted < retry_epoch
                    && retry_deadline.is_some_and(|deadline| std::time::Instant::now() < deadline)
            });
            if workers.len() >= CREDENTIAL_RELEASE_FINALIZER_MAX_WORKERS || !retry_is_authorized {
                pending.push_back(task);
                continue;
            }
            let attempt_epoch = retry_epoch;
            let callback_deadline = task.last_attempt_epoch.and(retry_deadline);
            let scope = task.owner.scope;
            match spawn_deferred_credential_release(task.owner, callback_deadline) {
                Ok(worker) => {
                    update_credential_finalizer_progress(&shared, scope, |progress| {
                        progress.pending = progress.pending.saturating_sub(1);
                        progress.running = progress.running.saturating_add(1);
                    });
                    workers.push(RunningCredentialRelease {
                        worker,
                        attempt_epoch,
                        scope,
                    });
                }
                Err(owner) => {
                    task = PendingCredentialRelease {
                        owner,
                        last_attempt_epoch: Some(attempt_epoch),
                    };
                    pending.push_back(task);
                }
            }
        }

        for acknowledgement in acknowledgements {
            let _ = acknowledgement.send(());
        }
    }
}

fn apply_credential_finalizer_command(
    command: CredentialFinalizerCommand,
    pending: &mut VecDeque<PendingCredentialRelease>,
    retry_authorizations: &mut BTreeMap<u64, (u64, Option<std::time::Instant>)>,
    acknowledgements: &mut Vec<mpsc::SyncSender<()>>,
) {
    match command {
        CredentialFinalizerCommand::Enqueue(owner) => {
            pending.push_back(PendingCredentialRelease {
                owner,
                last_attempt_epoch: None,
            });
        }
        CredentialFinalizerCommand::Retry {
            scope,
            deadline,
            acknowledgement,
        } => {
            let authorization = retry_authorizations.entry(scope).or_insert((0, None));
            authorization.0 = authorization.0.saturating_add(1);
            authorization.1 = Some(deadline);
            acknowledgements.push(acknowledgement);
        }
    }
}

fn reap_credential_release_workers(
    workers: &mut Vec<RunningCredentialRelease>,
    pending: &mut VecDeque<PendingCredentialRelease>,
    shared: &CredentialFinalizerShared,
) {
    let mut index = 0;
    while index < workers.len() {
        if !workers[index].worker.is_finished() {
            index += 1;
            continue;
        }
        let task = workers.swap_remove(index);
        match task.worker.join() {
            Ok(CredentialReleaseOutcome::Released) => {
                update_credential_finalizer_progress(shared, task.scope, |progress| {
                    progress.running = progress.running.saturating_sub(1);
                    progress.outstanding = progress.outstanding.saturating_sub(1);
                });
            }
            Ok(CredentialReleaseOutcome::Failed(owner)) => {
                pending.push_back(PendingCredentialRelease {
                    owner,
                    last_attempt_epoch: Some(task.attempt_epoch),
                });
                update_credential_finalizer_progress(shared, task.scope, |progress| {
                    progress.running = progress.running.saturating_sub(1);
                    progress.pending = progress.pending.saturating_add(1);
                });
            }
            Err(_) => {
                update_credential_finalizer_progress(shared, task.scope, |progress| {
                    progress.running = progress.running.saturating_sub(1);
                    progress.quarantined = progress.quarantined.saturating_add(1);
                });
            }
        }
    }
}

fn update_credential_finalizer_progress(
    shared: &CredentialFinalizerShared,
    scope: u64,
    update: impl FnOnce(&mut CredentialFinalizerProgress),
) {
    let mut progress_by_scope = shared
        .progress
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    update(progress_by_scope.entry(scope).or_default());
    drop(progress_by_scope);
    shared.changed.notify_all();
}

fn spawn_deferred_credential_release(
    owner: DeferredCredentialSession,
    callback_deadline: Option<std::time::Instant>,
) -> Result<thread::JoinHandle<CredentialReleaseOutcome>, DeferredCredentialSession> {
    let owner = Arc::new(Mutex::new(Some(owner)));
    let worker_owner = Arc::clone(&owner);
    let worker = thread::Builder::new()
        .name("eva-credential-finalizer-release".to_owned())
        .spawn(move || {
            let mut owner = worker_owner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
                .expect("credential finalizer worker owns one session");
            if callback_deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                return CredentialReleaseOutcome::Failed(owner);
            }
            let released = catch_unwind(AssertUnwindSafe(|| owner.session.release()));
            if matches!(released, Ok(Ok(()))) {
                let _ = catch_unwind(AssertUnwindSafe(|| drop(owner)));
                CredentialReleaseOutcome::Released
            } else {
                CredentialReleaseOutcome::Failed(owner)
            }
        });

    match worker {
        Ok(worker) => Ok(worker),
        Err(_) => Err(owner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .expect("failed credential worker spawn preserves its session")),
    }
}

fn defer_credential_release(session: Box<dyn CredentialSession>, scope: u64) {
    let owner = DeferredCredentialSession { session, scope };
    let Ok(finalizer) = credential_release_finalizer() else {
        let _ = Box::leak(Box::new(owner));
        return;
    };
    update_credential_finalizer_progress(&finalizer.shared, scope, |progress| {
        progress.outstanding = progress.outstanding.saturating_add(1);
        progress.pending = progress.pending.saturating_add(1);
    });
    if let Err(error) = finalizer
        .sender
        .send(CredentialFinalizerCommand::Enqueue(owner))
    {
        if let CredentialFinalizerCommand::Enqueue(owner) = error.0 {
            let _ = Box::leak(Box::new(owner));
        }
    }
}

pub(crate) fn track_running_credential_finalizer_owner(
    scope: u64,
) -> Option<CredentialFinalizerRunningGuard> {
    let finalizer = CREDENTIAL_RELEASE_FINALIZER.get()?;
    update_credential_finalizer_progress(&finalizer.shared, scope, |progress| {
        progress.outstanding = progress.outstanding.saturating_add(1);
        progress.running = progress.running.saturating_add(1);
    });
    Some(CredentialFinalizerRunningGuard {
        shared: Arc::clone(&finalizer.shared),
        scope,
    })
}

impl Drop for CredentialFinalizerRunningGuard {
    fn drop(&mut self) {
        update_credential_finalizer_progress(&self.shared, self.scope, |progress| {
            progress.running = progress.running.saturating_sub(1);
            progress.outstanding = progress.outstanding.saturating_sub(1);
        });
    }
}

pub(crate) fn drain_deferred_credential_releases_until(
    deadline: std::time::Instant,
) -> Result<(), EvaError> {
    drain_deferred_credential_releases_until_inner(deadline, credential_finalizer_scope())
}

fn drain_deferred_credential_releases_until_inner(
    deadline: std::time::Instant,
    scope: u64,
) -> Result<(), EvaError> {
    let Some(finalizer) = CREDENTIAL_RELEASE_FINALIZER.get() else {
        return Ok(());
    };
    {
        let progress_by_scope = finalizer
            .shared
            .progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let progress = progress_by_scope.get(&scope).copied().unwrap_or_default();
        if progress.outstanding == 0 {
            return if std::time::Instant::now() < deadline {
                Ok(())
            } else {
                Err(credential_finalizer_timeout(progress))
            };
        }
    }

    let (acknowledgement, acknowledged) = mpsc::sync_channel(1);
    finalizer
        .sender
        .send(CredentialFinalizerCommand::Retry {
            scope,
            deadline,
            acknowledgement,
        })
        .map_err(|_| credential_finalizer_unavailable(finalizer, scope))?;
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() || acknowledged.recv_timeout(remaining).is_err() {
        return Err(credential_finalizer_timeout_snapshot(finalizer, scope));
    }

    let mut progress_by_scope = finalizer
        .shared
        .progress
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    loop {
        let progress = progress_by_scope.get(&scope).copied().unwrap_or_default();
        if std::time::Instant::now() >= deadline {
            return Err(credential_finalizer_timeout(progress));
        }
        if progress.outstanding == 0 {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let (next, _) = finalizer
            .shared
            .changed
            .wait_timeout(progress_by_scope, remaining)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        progress_by_scope = next;
    }
}

#[cfg(test)]
pub(crate) fn credential_finalizer_test_guard() -> CredentialFinalizerTestGuard {
    let scope =
        NEXT_CREDENTIAL_FINALIZER_TEST_SCOPE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let previous_scope = CREDENTIAL_FINALIZER_TEST_SCOPE.with(|current| current.replace(scope));
    CredentialFinalizerTestGuard { previous_scope }
}

#[cfg(test)]
impl Drop for CredentialFinalizerTestGuard {
    fn drop(&mut self) {
        CREDENTIAL_FINALIZER_TEST_SCOPE.with(|current| current.set(self.previous_scope));
    }
}

#[cfg(test)]
fn credential_finalizer_scope() -> u64 {
    CREDENTIAL_FINALIZER_TEST_SCOPE.with(std::cell::Cell::get)
}

#[cfg(not(test))]
const fn credential_finalizer_scope() -> u64 {
    0
}

#[cfg(test)]
pub(crate) fn drain_credential_finalizer_while_test_guarded(
    deadline: std::time::Instant,
) -> Result<(), EvaError> {
    drain_deferred_credential_releases_until_inner(deadline, credential_finalizer_scope())
}

fn credential_finalizer_timeout_snapshot(
    finalizer: &CredentialReleaseFinalizer,
    scope: u64,
) -> EvaError {
    let progress_by_scope = finalizer
        .shared
        .progress
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    credential_finalizer_timeout(progress_by_scope.get(&scope).copied().unwrap_or_default())
}

fn credential_finalizer_timeout(progress: CredentialFinalizerProgress) -> EvaError {
    EvaError::timeout("provider credential finalizer exceeded the drain deadline")
        .with_provider_code("credential_finalizer_drain_timeout")
        .with_retryable(true)
        .with_context(
            "credential_finalizer_outstanding",
            progress.outstanding.to_string(),
        )
        .with_context("credential_finalizer_pending", progress.pending.to_string())
        .with_context("credential_finalizer_running", progress.running.to_string())
        .with_context(
            "credential_finalizer_quarantined",
            progress.quarantined.to_string(),
        )
        .with_context("cleanup_blocked", (progress.outstanding != 0).to_string())
}

fn credential_finalizer_unavailable(
    finalizer: &CredentialReleaseFinalizer,
    scope: u64,
) -> EvaError {
    let progress_by_scope = finalizer
        .shared
        .progress
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let progress = progress_by_scope.get(&scope).copied().unwrap_or_default();
    EvaError::unavailable("provider credential finalizer is unavailable")
        .with_provider_code("credential_finalizer_unavailable")
        .with_retryable(true)
        .with_context(
            "credential_finalizer_outstanding",
            progress.outstanding.to_string(),
        )
        .with_context("credential_finalizer_pending", progress.pending.to_string())
        .with_context("credential_finalizer_running", progress.running.to_string())
        .with_context("cleanup_blocked", (progress.outstanding != 0).to_string())
}

pub(crate) struct CredentialSessionLeaseOpenFailure {
    error: EvaError,
    lease: Option<Box<CredentialSessionLease>>,
}

impl CredentialSessionLeaseOpenFailure {
    pub(crate) fn into_parts(self) -> (EvaError, Option<CredentialSessionLease>) {
        (self.error, self.lease.map(|lease| *lease))
    }
}

impl fmt::Debug for CredentialSessionLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialSessionLease")
            .field("env_names", &self.values.keys().collect::<Vec<_>>())
            .field("secret_count", &self.values.len())
            .field("released", &self.released)
            .finish()
    }
}

impl CredentialSessionLease {
    /// Open and populate a session from canonical vault refs plus legacy
    /// allowlisted env names.  Legacy names are resolved by the vault too;
    /// this function never reads the parent process environment.
    pub fn open(
        vault: &dyn CredentialVault,
        scope: Option<&ProviderCredentialScope>,
        vault_refs: &[ProviderVaultSecretRef],
        legacy_env: &[String],
    ) -> Result<Self, EvaError> {
        Self::open_with_lazy_env(vault, scope, vault_refs, legacy_env, &[])
    }

    /// Open a session while deferring selected environment references until a
    /// transport resolves a header lazily.  This keeps missing-header errors
    /// at the transport boundary while still ensuring the value comes from
    /// the vault rather than the parent process environment.
    pub(crate) fn open_with_lazy_env(
        vault: &dyn CredentialVault,
        scope: Option<&ProviderCredentialScope>,
        vault_refs: &[ProviderVaultSecretRef],
        legacy_env: &[String],
        lazy_env: &[String],
    ) -> Result<Self, EvaError> {
        match Self::open_with_lazy_env_retaining_owner(
            vault, scope, vault_refs, legacy_env, lazy_env,
        ) {
            Ok(lease) => Ok(lease),
            Err(failure) => {
                let (mut error, lease) = failure.into_parts();
                if let Some(mut lease) = lease {
                    if let Err(release_error) = lease.release() {
                        error = error
                            .with_context("credential_release_error", release_error.to_string());
                    }
                }
                Err(error)
            }
        }
    }

    pub(crate) fn open_with_lazy_env_retaining_owner(
        vault: &dyn CredentialVault,
        scope: Option<&ProviderCredentialScope>,
        vault_refs: &[ProviderVaultSecretRef],
        legacy_env: &[String],
        lazy_env: &[String],
    ) -> Result<Self, CredentialSessionLeaseOpenFailure> {
        if vault_refs.is_empty() && legacy_env.is_empty() && lazy_env.is_empty() {
            return Ok(Self {
                session: None,
                values: BTreeMap::new(),
                allowed_envs: BTreeSet::new(),
                redactions: Vec::new(),
                audit: Vec::new(),
                released: false,
                finalizer_scope: credential_finalizer_scope(),
            });
        }
        let scope = scope.ok_or_else(|| CredentialSessionLeaseOpenFailure {
            error: EvaError::permission_denied("provider credential session scope is required")
                .with_provider_code("credential_scope_required"),
            lease: None,
        })?;
        credential_release_finalizer()
            .map_err(|error| CredentialSessionLeaseOpenFailure { error, lease: None })?;
        let session =
            vault
                .open_session(scope)
                .map_err(|error| CredentialSessionLeaseOpenFailure {
                    error: sanitize_vault_error(
                        error,
                        "provider credential vault session unavailable",
                        "credential_vault_error",
                    ),
                    lease: None,
                })?;
        let mut allowed_envs = BTreeSet::new();
        allowed_envs.extend(vault_refs.iter().map(|reference| reference.env.clone()));
        allowed_envs.extend(legacy_env.iter().cloned());
        allowed_envs.extend(lazy_env.iter().cloned());
        let mut lease = Self {
            session: Some(session),
            values: BTreeMap::new(),
            allowed_envs,
            redactions: Vec::new(),
            audit: vec!["credential.vault_session:opened".to_owned()],
            released: false,
            finalizer_scope: credential_finalizer_scope(),
        };

        for reference in vault_refs {
            let secret = match lease
                .session
                .as_mut()
                .expect("new credential lease owns its vault session")
                .fetch(&reference.secret_ref)
            {
                Ok(secret) => secret,
                Err(error) => {
                    return Err(CredentialSessionLeaseOpenFailure {
                        error: sanitize_vault_error(
                            error,
                            "provider credential reference is unavailable",
                            "missing_credential",
                        )
                        .with_context("credential_env", &reference.env),
                        lease: Some(Box::new(lease)),
                    });
                }
            };
            if secret.expose().is_empty() {
                return Err(CredentialSessionLeaseOpenFailure {
                    error: EvaError::unavailable(
                        "provider credential reference resolved to an empty value",
                    )
                    .with_provider_code("empty_credential")
                    .with_context("credential_env", &reference.env),
                    lease: Some(Box::new(lease)),
                });
            }
            lease
                .values
                .insert(reference.env.clone(), secret.expose().to_owned());
            lease.redactions.push(secret.expose().to_owned());
            lease
                .audit
                .push(format!("credential_vault:{}:redacted", reference.env));
        }

        for name in legacy_env {
            if lease.values.contains_key(name) {
                continue;
            }
            let secret = match lease
                .session
                .as_mut()
                .expect("new credential lease owns its vault session")
                .fetch(name)
            {
                Ok(secret) => secret,
                Err(error) => {
                    return Err(CredentialSessionLeaseOpenFailure {
                        error: sanitize_vault_error(
                            error,
                            "provider credential reference is unavailable",
                            "missing_credential",
                        )
                        .with_context("credential_env", name),
                        lease: Some(Box::new(lease)),
                    });
                }
            };
            if secret.expose().is_empty() {
                return Err(CredentialSessionLeaseOpenFailure {
                    error: EvaError::unavailable(
                        "provider credential environment reference resolved to an empty value",
                    )
                    .with_provider_code("empty_credential")
                    .with_context("credential_env", name),
                    lease: Some(Box::new(lease)),
                });
            }
            lease
                .values
                .insert(name.clone(), secret.expose().to_owned());
            lease.redactions.push(secret.expose().to_owned());
            lease.audit.push(format!("credential_env:{name}:redacted"));
        }

        Ok(lease)
    }

    /// Inject fetched values into an explicit child environment map.
    pub(crate) fn inject_env(&self, env_values: &mut BTreeMap<String, String>) {
        if !self.released {
            env_values.extend(self.values.clone());
        }
    }

    /// Resolve one allowlisted env reference for an HTTP header.
    pub(crate) fn resolve_env(&mut self, name: &str) -> Result<String, EvaError> {
        if self.released {
            return Err(EvaError::conflict(
                "provider credential session has been released",
            ));
        }
        if !self.allowed_envs.contains(name) {
            return Err(EvaError::permission_denied(
                "provider credential environment reference is not declared",
            )
            .with_provider_code("credential_env_not_allowlisted")
            .with_context("credential_env", name));
        }
        if let Some(value) = self.values.get(name) {
            return Ok(value.clone());
        }
        let Some(session) = self.session.as_mut() else {
            return Err(
                EvaError::permission_denied("provider credential session is unavailable")
                    .with_provider_code("credential_vault_unconfigured"),
            );
        };
        let secret = session.fetch(name).map_err(|error| {
            sanitize_vault_error(
                error,
                "provider credential reference is unavailable",
                "missing_credential",
            )
            .with_context("credential_env", name)
        })?;
        if secret.expose().is_empty() {
            return Err(EvaError::unavailable(
                "provider credential environment reference resolved to an empty value",
            )
            .with_provider_code("empty_credential")
            .with_context("credential_env", name));
        }
        self.values
            .insert(name.to_owned(), secret.expose().to_owned());
        self.redactions.push(secret.expose().to_owned());
        self.audit.push(format!("credential_env:{name}:redacted"));
        Ok(secret.expose().to_owned())
    }

    /// Resolve an MCP client-auth reference through this invocation's
    /// allowlisted environment projection. Vault references must map to one
    /// and only one declared provider secret; the opaque reference is never
    /// copied into an error or audit record.
    pub(crate) fn resolve_reference(
        &mut self,
        reference: &str,
        vault_refs: &[ProviderVaultSecretRef],
    ) -> Result<String, EvaError> {
        if let Some(name) = reference.strip_prefix("env:") {
            return self.resolve_env(name);
        }
        if reference.starts_with("vault://") {
            let mut matches = vault_refs
                .iter()
                .filter(|candidate| candidate.secret_ref == reference);
            let Some(candidate) = matches.next() else {
                return Err(EvaError::permission_denied(
                    "MCP client-auth vault reference is not declared",
                )
                .with_provider_code("credential_ref_not_declared"));
            };
            if matches.next().is_some() {
                return Err(
                    EvaError::conflict("MCP client-auth vault reference is ambiguous")
                        .with_provider_code("credential_ref_ambiguous"),
                );
            }
            let env = candidate.env.clone();
            return self.resolve_env(&env);
        }
        Err(
            EvaError::permission_denied("unsupported MCP client-auth credential reference")
                .with_provider_code("credential_ref_unsupported"),
        )
    }

    /// Values that must be removed from stream previews, artifacts, and errors.
    pub(crate) fn redaction_values(&self) -> Vec<String> {
        if self.released {
            Vec::new()
        } else {
            self.redactions.clone()
        }
    }

    /// Stable audit entries with names only; no bytes are returned.
    pub(crate) fn audit_entries(&self) -> Vec<String> {
        self.audit.clone()
    }

    pub(crate) const fn release_finalizer_scope(&self) -> u64 {
        self.finalizer_scope
    }

    /// Explicitly close the provider session and clear the local projection.
    pub fn release(&mut self) -> Result<(), EvaError> {
        if self.released {
            return Ok(());
        }
        if let Some(session) = self.session.as_mut() {
            session.release().map_err(|error| {
                sanitize_vault_error(
                    error,
                    "provider credential vault session release failed",
                    "credential_vault_release_error",
                )
            })?;
        }
        self.released = true;
        self.session = None;
        self.wipe_local_projection();
        self.audit
            .push("credential.vault_session:released".to_owned());
        Ok(())
    }

    fn wipe_local_projection(&mut self) {
        wipe_map_values(&mut self.values);
        self.allowed_envs.clear();
        for value in &mut self.redactions {
            wipe_string(value);
        }
        self.redactions.clear();
    }
}

/// Best-effort in-place clearing for transient secret strings.  The public
/// API never promises allocator-level zeroization, but release paths should
/// overwrite bytes before dropping their owned buffers whenever possible.
fn wipe_string(value: &mut String) {
    if value.is_empty() {
        return;
    }
    let zeros = "\0".repeat(value.len());
    value.replace_range(.., &zeros);
    value.clear();
}

fn wipe_map_values(values: &mut BTreeMap<String, String>) {
    for value in values.values_mut() {
        wipe_string(value);
    }
    values.clear();
}

/// Vault implementations are an authority boundary and may be supplied by
/// external code.  Never propagate their free-form message or context because
/// either can accidentally contain secret bytes; preserve only classification
/// and retryability plus a stable, non-sensitive provider code.
fn sanitize_vault_error(error: EvaError, message: &'static str, code: &'static str) -> EvaError {
    EvaError::new(error.kind(), message)
        .with_retryable(error.is_retryable())
        .with_provider_code(code)
}

/// Redact vault values and provider session tokens from an error before it is
/// returned to runtime, audit, or observability layers.  Provider transports
/// may receive errors from external clients, so their free-form text is not a
/// trusted boundary even when the transport itself never logs the request.
pub(crate) fn sanitize_error_with_values(error: EvaError, sensitive_values: &[String]) -> EvaError {
    let redact = |value: &str| {
        let mut redacted = value.to_owned();
        for secret in sensitive_values {
            if !secret.is_empty() {
                redacted = redacted.replace(secret, "[REDACTED]");
            }
        }
        redact_provider_session_tokens(&redacted)
    };
    let mut safe =
        EvaError::new(error.kind(), redact(error.message())).with_retryable(error.is_retryable());
    if let Some(code) = error.provider_code() {
        safe = safe.with_provider_code(redact(code.as_str()));
    }
    for (key, value) in error.context().entries() {
        safe = safe.with_context(redact(key), redact(value));
    }
    safe
}

impl Drop for CredentialSessionLease {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let session = self.session.take();
        self.wipe_local_projection();
        self.audit.clear();
        if let Some(session) = session {
            defer_credential_release(session, self.finalizer_scope);
        }
    }
}

/// Return the smallest inherited environment needed to locate a provider
/// executable.  Secret-bearing names are never copied from the parent.
pub(crate) fn minimal_process_env(explicit: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for name in ["PATH", "SystemRoot", "SYSTEMROOT", "WINDIR"] {
        if let Ok(value) = env::var(name) {
            values.insert(name.to_owned(), value);
        }
    }
    #[cfg(test)]
    for name in ["EVA_RESTART_COUNTER_FILE", "EVA_OUTPUT_LIMIT_COUNTER_FILE"] {
        if let Ok(value) = env::var(name) {
            values.insert(name.to_owned(), value);
        }
    }
    values.extend(explicit.clone());
    values
}

/// Select the default vault for a direct transport call.  The default is
/// always fail-closed; tests and daemon hosts must wire an explicit vault.
pub(crate) fn default_credential_vault() -> Box<dyn CredentialVault> {
    Box::new(FailClosedCredentialVault)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::{AdapterId, CapabilityName, ErrorKind, RequestId};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scope() -> ProviderCredentialScope {
        ProviderCredentialScope::new_for_session(
            "vault-test-session",
            AdapterId::parse("vault-test-provider").unwrap(),
            RequestId::parse("req-vault-test").unwrap(),
            CapabilityName::parse("vault.read").unwrap(),
        )
    }

    #[test]
    fn fail_closed_vault_never_reads_parent_environment() {
        let vault = FailClosedCredentialVault;
        let error = vault.open_session(&scope()).unwrap_err();
        assert_eq!(
            error.provider_code().unwrap().as_str(),
            "credential_vault_unconfigured"
        );
    }

    #[test]
    fn memory_session_fetch_inject_release_and_debug_redact() {
        let vault = MemoryCredentialVault::new().with_secret("vault://tests/token", "top-secret");
        let refs = vec![ProviderVaultSecretRef {
            env: "API_TOKEN".to_owned(),
            secret_ref: "vault://tests/token".to_owned(),
        }];
        let mut lease = CredentialSessionLease::open(&vault, Some(&scope()), &refs, &[]).unwrap();
        let mut env_values = BTreeMap::new();
        lease.inject_env(&mut env_values);
        assert_eq!(
            env_values.get("API_TOKEN").map(String::as_str),
            Some("top-secret")
        );
        assert!(!format!("{lease:?}").contains("top-secret"));
        assert!(lease.release().is_ok());
        assert!(lease.resolve_env("API_TOKEN").is_err());
        assert!(lease.redaction_values().is_empty());
        drop(env_values);
    }

    #[test]
    fn lazy_environment_resolution_is_allowlisted() {
        let vault = MemoryCredentialVault::new().with_secret("API_TOKEN", "top-secret");
        let mut lease = CredentialSessionLease::open_with_lazy_env(
            &vault,
            Some(&scope()),
            &[],
            &[],
            &["API_TOKEN".to_owned()],
        )
        .unwrap();
        assert_eq!(lease.resolve_env("API_TOKEN").unwrap(), "top-secret");
        let error = lease.resolve_env("UNDECLARED_TOKEN").unwrap_err();
        assert_eq!(
            error.provider_code().unwrap().as_str(),
            "credential_env_not_allowlisted"
        );
        lease.release().unwrap();
    }

    #[test]
    fn client_auth_reference_requires_one_declared_mapping() {
        let secret_ref = "vault://tests/client-cert";
        let vault = MemoryCredentialVault::new().with_secret(secret_ref, "client-cert-pem");
        let refs = vec![ProviderVaultSecretRef {
            env: "CLIENT_CERT".to_owned(),
            secret_ref: secret_ref.to_owned(),
        }];
        let mut lease = CredentialSessionLease::open(&vault, Some(&scope()), &refs, &[]).unwrap();

        assert_eq!(
            lease.resolve_reference(secret_ref, &refs).unwrap(),
            "client-cert-pem"
        );
        let missing = lease
            .resolve_reference("vault://tests/not-declared", &refs)
            .unwrap_err();
        assert_eq!(
            missing.provider_code().map(|code| code.as_str()),
            Some("credential_ref_not_declared")
        );

        let duplicate = vec![
            refs[0].clone(),
            ProviderVaultSecretRef {
                env: "SECOND_CERT".to_owned(),
                secret_ref: secret_ref.to_owned(),
            },
        ];
        let ambiguous = lease.resolve_reference(secret_ref, &duplicate).unwrap_err();
        assert_eq!(
            ambiguous.provider_code().map(|code| code.as_str()),
            Some("credential_ref_ambiguous")
        );
        assert!(!format!("{missing:?}{ambiguous:?}").contains(secret_ref));
        lease.release().unwrap();
    }

    #[test]
    fn missing_scope_is_rejected_before_vault_access() {
        let vault = MemoryCredentialVault::new().with_secret("vault://tests/token", "top-secret");
        let refs = vec![ProviderVaultSecretRef {
            env: "API_TOKEN".to_owned(),
            secret_ref: "vault://tests/token".to_owned(),
        }];
        let error = CredentialSessionLease::open(&vault, None, &refs, &[]).unwrap_err();
        assert_eq!(
            error.provider_code().unwrap().as_str(),
            "credential_scope_required"
        );
    }

    #[test]
    fn secret_value_formatting_is_always_redacted() {
        let secret = SecretValue::new("vault-test-secret");
        assert_eq!(format!("{secret:?}"), "[REDACTED]");
        assert_eq!(secret.to_string(), "[REDACTED]");
        let error = EvaError::internal(
            "vault-test-secret eva-provider-session:sess:digest and vault-test-secret",
        );
        let safe = sanitize_error_with_values(error, &["vault-test-secret".to_owned()]);
        assert!(!format!("{safe:?}").contains("vault-test-secret"));
        assert!(!safe.message().contains("eva-provider-session:"));
    }

    #[test]
    fn vault_errors_are_sanitized_and_failed_release_is_retried() {
        let releases = Arc::new(AtomicUsize::new(0));
        let vault = CountingVault {
            releases: releases.clone(),
            secret: "external-vault-secret".to_owned(),
            fail_fetch: true,
            release_failures_remaining: 0,
        };
        let refs = vec![ProviderVaultSecretRef {
            env: "API_TOKEN".to_owned(),
            secret_ref: "vault://tests/missing".to_owned(),
        }];
        let error = CredentialSessionLease::open(&vault, Some(&scope()), &refs, &[]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert!(!format!("{error:?}").contains("external-vault-secret"));
        assert_eq!(releases.load(Ordering::SeqCst), 1);

        let releases = Arc::new(AtomicUsize::new(0));
        let vault = CountingVault {
            releases: releases.clone(),
            secret: "external-vault-secret".to_owned(),
            fail_fetch: false,
            release_failures_remaining: 1,
        };
        let mut lease =
            CredentialSessionLease::open(&vault, Some(&scope()), &[], &["API_TOKEN".to_owned()])
                .unwrap();
        let error = lease.release().unwrap_err();
        assert_eq!(
            error.provider_code().unwrap().as_str(),
            "credential_vault_release_error"
        );
        assert!(!format!("{error:?}").contains("external-vault-secret"));
        assert_eq!(
            lease.resolve_env("API_TOKEN").unwrap(),
            "external-vault-secret"
        );
        assert!(lease.release().is_ok());
        assert!(lease.release().is_ok());
        drop(lease);
        assert_eq!(releases.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn expired_retry_worker_preserves_owner_without_entering_release_callback() {
        let releases = Arc::new(AtomicUsize::new(0));
        let owner = DeferredCredentialSession {
            session: Box::new(CountingSession {
                releases: Arc::clone(&releases),
                secret: "expired-retry-secret".to_owned(),
                fail_fetch: false,
                release_failures_remaining: 0,
                released: false,
            }),
            scope: 0,
        };

        let worker = match spawn_deferred_credential_release(owner, Some(std::time::Instant::now()))
        {
            Ok(worker) => worker,
            Err(_) => panic!("the retry worker should start"),
        };
        let mut owner = match worker.join().expect("the retry worker should not panic") {
            CredentialReleaseOutcome::Failed(owner) => owner,
            CredentialReleaseOutcome::Released => {
                panic!("an expired retry entered the credential release callback")
            }
        };

        assert_eq!(releases.load(Ordering::SeqCst), 0);
        owner.session.release().unwrap();
        assert_eq!(releases.load(Ordering::SeqCst), 1);
    }

    #[derive(Debug, Clone)]
    struct CountingVault {
        releases: Arc<AtomicUsize>,
        secret: String,
        fail_fetch: bool,
        release_failures_remaining: usize,
    }

    impl CredentialVault for CountingVault {
        fn open_session(
            &self,
            _scope: &ProviderCredentialScope,
        ) -> Result<Box<dyn CredentialSession>, EvaError> {
            Ok(Box::new(CountingSession {
                releases: self.releases.clone(),
                secret: self.secret.clone(),
                fail_fetch: self.fail_fetch,
                release_failures_remaining: self.release_failures_remaining,
                released: false,
            }))
        }
    }

    #[derive(Clone)]
    struct CountingSession {
        releases: Arc<AtomicUsize>,
        secret: String,
        fail_fetch: bool,
        release_failures_remaining: usize,
        released: bool,
    }

    impl fmt::Debug for CountingSession {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("CountingSession")
                .field("released", &self.released)
                .finish()
        }
    }

    impl CredentialSession for CountingSession {
        fn fetch(&mut self, _secret_ref: &str) -> Result<SecretValue, EvaError> {
            if self.fail_fetch {
                return Err(EvaError::unavailable(format!(
                    "vault backend rejected {}",
                    self.secret
                )));
            }
            Ok(SecretValue::new(self.secret.clone()))
        }

        fn release(&mut self) -> Result<(), EvaError> {
            if self.released {
                return Ok(());
            }
            self.releases.fetch_add(1, Ordering::SeqCst);
            if self.release_failures_remaining > 0 {
                self.release_failures_remaining -= 1;
                return Err(EvaError::internal(format!(
                    "vault release failed for {}",
                    self.secret
                )));
            }
            self.released = true;
            Ok(())
        }
    }
}
