//! Durable task handler lookup and payload integrity boundary.

use crate::{TaskArtifactRef, TaskEnvelope, TaskInput, TaskKind};
use eva_core::{EvaError, RequestId};
use eva_storage::{
    artifact_store::sha256_digest, ArtifactRecord, ArtifactStore, FileSystemArtifactStore,
    InMemoryArtifactStore,
};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

/// This module owns handler registration and one synchronous dispatch boundary.
pub const RESPONSIBILITY: &str = "task handler registration and payload integrity dispatch";

/// Stable human-readable message for a syntactically valid but unregistered task kind.
pub const TASK_HANDLER_NOT_REGISTERED_MESSAGE: &str = "task handler is not registered";

/// Default upper bound for one artifact-backed task input read by the daemon.
pub const DEFAULT_TASK_ARTIFACT_INPUT_LIMIT_BYTES: usize = 16 * 1024 * 1024;

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
        };
        handler.handle(&invocation).map_err(|error| {
            error
                .with_context("task_id", task_id.as_str())
                .with_context("task_kind", envelope.kind().as_str())
                .with_context("agent_id", envelope.agent_id().as_str())
        })
    }
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

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
        let invocation = TaskHandlerInvocation {
            task_id: &id,
            envelope: &task,
            payload,
            payload_digest: &digest,
        };
        let result = TaskHandlerResult::new(payload.as_slice());

        let invocation_debug = format!("{invocation:?}");
        let result_debug = format!("{result:?}");
        assert!(invocation_debug.contains("<redacted>"));
        assert!(result_debug.contains("<redacted>"));
        assert!(!invocation_debug.contains("private-task-input"));
        assert!(!result_debug.contains("private-task-input"));
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
}
