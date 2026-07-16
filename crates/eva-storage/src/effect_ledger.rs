//! Durable commit boundary for non-idempotent task-handler effects.

use crate::{
    atomic_write, DurableBackendLayout, DurableWriterGuard, FileSystemTaskStateStore,
    TaskStateStore, WriterGeneration,
};
use eva_core::{AgentId, EvaError, RequestId};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const EFFECT_LEDGER_DIR: &str = "effects";
const EFFECT_RECORD_EXTENSION: &str = "effect";
const EFFECT_LEDGER_FORMAT: &str = "eva.effect-ledger.v1";
const EFFECT_RECORD_FIELD_COUNT: usize = 16;
const MAX_EFFECT_FENCE_FIELD_BYTES: usize = 512;

/// Monotonic durable state of one non-idempotent operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectLedgerState {
    /// The operation may have started, so automatic execution is no longer safe.
    Prepared,
    /// The operation completed and its result identity is durable.
    Committed,
}

/// Immutable business identity protected by one idempotency key.
#[derive(Clone, PartialEq, Eq)]
pub struct EffectOperationIdentity {
    idempotency_key: String,
    operation_digest: String,
    task_kind: String,
    agent_id: String,
    effect_scope: String,
    input_digest: String,
}

/// Exact task attempt authorized to advance a prepared operation to committed.
#[derive(Clone, PartialEq, Eq)]
pub struct EffectLedgerIntent {
    operation: EffectOperationIdentity,
    task_id: String,
    owner_generation: WriterGeneration,
    attempt: usize,
    fence_digest: String,
    prepared_at_ms: u128,
}

/// Durable prepared/committed record. Result bytes and raw task fence tokens are never stored.
#[derive(Clone, PartialEq, Eq)]
pub struct EffectLedgerRecord {
    operation: EffectOperationIdentity,
    state: EffectLedgerState,
    prepared_task_id: String,
    prepared_owner_generation: WriterGeneration,
    prepared_attempt: usize,
    prepared_fence_digest: String,
    prepared_at_ms: u128,
    committed_at_ms: Option<u128>,
    result_digest: Option<String>,
    result_size_bytes: Option<usize>,
}

/// Result of a serialized prepare attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectPrepareOutcome {
    /// This caller created the prepared boundary and may invoke the handler once.
    Created(EffectLedgerRecord),
    /// A prepared boundary already exists; its external outcome is unknown.
    Prepared(EffectLedgerRecord),
    /// The effect is already committed and the handler must not run again.
    Committed(EffectLedgerRecord),
}

/// Filesystem effect ledger rooted under the durable backend state subtree.
pub struct FileSystemEffectLedger {
    root: PathBuf,
    records: BTreeMap<String, EffectLedgerRecord>,
    writer: Option<DurableWriterGuard>,
}

impl EffectLedgerState {
    /// Stable disk representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Committed => "committed",
        }
    }

    fn from_storage(value: &str) -> Result<Self, EvaError> {
        match value {
            "prepared" => Ok(Self::Prepared),
            "committed" => Ok(Self::Committed),
            _ => {
                Err(EvaError::conflict("effect ledger state is invalid")
                    .with_context("state", value))
            }
        }
    }
}

impl EffectOperationIdentity {
    /// Builds and validates the immutable identity for one idempotency key.
    pub fn new(
        idempotency_key: impl Into<String>,
        task_kind: impl Into<String>,
        agent_id: impl Into<String>,
        effect_scope: impl Into<String>,
        input_digest: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let idempotency_key = idempotency_key.into();
        let task_kind = task_kind.into();
        let agent_id = agent_id.into();
        let effect_scope = effect_scope.into();
        let input_digest = input_digest.into();
        RequestId::parse(&idempotency_key).map_err(|error| {
            EvaError::invalid_argument("effect idempotency key is invalid")
                .with_context("cause", error.message())
        })?;
        crate::TaskEnvelopeSnapshot::validate_kind(&task_kind)?;
        AgentId::parse(&agent_id)?;
        RequestId::parse(&effect_scope).map_err(|error| {
            EvaError::invalid_argument("effect scope is invalid")
                .with_context("cause", error.message())
        })?;
        validate_sha256_digest("input_digest", &input_digest)?;
        let operation_digest = canonical_digest(
            "eva.effect-operation.v1",
            &[
                &idempotency_key,
                &task_kind,
                &agent_id,
                &effect_scope,
                &input_digest,
            ],
        );
        Ok(Self {
            idempotency_key,
            operation_digest,
            task_kind,
            agent_id,
            effect_scope,
            input_digest,
        })
    }

    /// Stable idempotency key. Callers should avoid logging it as user-facing text.
    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }

    /// Canonical digest binding kind, Agent, input, and idempotency key.
    pub fn operation_digest(&self) -> &str {
        &self.operation_digest
    }

    /// Registered task kind whose effect is protected.
    pub fn task_kind(&self) -> &str {
        &self.task_kind
    }

    /// Agent identity whose handler owns the effect.
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Stable handler contract/effect-slot identity.
    pub fn effect_scope(&self) -> &str {
        &self.effect_scope
    }

    /// Canonical digest of the immutable task input.
    pub fn input_digest(&self) -> &str {
        &self.input_digest
    }
}

impl EffectLedgerIntent {
    /// Binds an operation prepare to one complete task-attempt fence without storing raw tokens.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        operation: EffectOperationIdentity,
        task_id: impl Into<String>,
        owner_generation: WriterGeneration,
        execution_owner: &str,
        attempt: usize,
        cancel_token: &str,
        prepared_at_ms: u128,
    ) -> Result<Self, EvaError> {
        let task_id = task_id.into();
        RequestId::parse(&task_id)?;
        if owner_generation == WriterGeneration::ZERO {
            return Err(EvaError::invalid_argument(
                "effect prepare requires a non-zero writer generation",
            ));
        }
        if attempt == 0 {
            return Err(EvaError::invalid_argument(
                "effect prepare requires a one-based task attempt",
            ));
        }
        validate_fence_field("execution_owner", execution_owner)?;
        validate_fence_field("cancel_token", cancel_token)?;
        let generation = owner_generation.0.to_string();
        let attempt_text = attempt.to_string();
        let fence_digest = canonical_digest(
            "eva.effect-attempt-fence.v1",
            &[
                &task_id,
                &generation,
                execution_owner,
                &attempt_text,
                cancel_token,
            ],
        );
        Ok(Self {
            operation,
            task_id,
            owner_generation,
            attempt,
            fence_digest,
            prepared_at_ms,
        })
    }

    /// Immutable business operation protected by this attempt.
    pub fn operation(&self) -> &EffectOperationIdentity {
        &self.operation
    }
}

impl EffectLedgerRecord {
    fn prepared(intent: &EffectLedgerIntent) -> Self {
        Self {
            operation: intent.operation.clone(),
            state: EffectLedgerState::Prepared,
            prepared_task_id: intent.task_id.clone(),
            prepared_owner_generation: intent.owner_generation,
            prepared_attempt: intent.attempt,
            prepared_fence_digest: intent.fence_digest.clone(),
            prepared_at_ms: intent.prepared_at_ms,
            committed_at_ms: None,
            result_digest: None,
            result_size_bytes: None,
        }
    }

    /// Business operation identity.
    pub fn operation(&self) -> &EffectOperationIdentity {
        &self.operation
    }

    /// Current monotonic ledger state.
    pub const fn state(&self) -> EffectLedgerState {
        self.state
    }

    /// Task that first established the prepare boundary.
    pub fn prepared_task_id(&self) -> &str {
        &self.prepared_task_id
    }

    /// Writer generation that first established the prepare boundary.
    pub const fn prepared_owner_generation(&self) -> WriterGeneration {
        self.prepared_owner_generation
    }

    /// One-based attempt that first established the prepare boundary.
    pub const fn prepared_attempt(&self) -> usize {
        self.prepared_attempt
    }

    /// Time at which the prepare boundary became durable.
    pub const fn prepared_at_ms(&self) -> u128 {
        self.prepared_at_ms
    }

    /// Time at which the committed result identity became durable.
    pub const fn committed_at_ms(&self) -> Option<u128> {
        self.committed_at_ms
    }

    /// Canonical committed result digest, absent while prepared.
    pub fn result_digest(&self) -> Option<&str> {
        self.result_digest.as_deref()
    }

    /// Committed result size, absent while prepared.
    pub const fn result_size_bytes(&self) -> Option<usize> {
        self.result_size_bytes
    }

    fn validate(&self) -> Result<(), EvaError> {
        let canonical = EffectOperationIdentity::new(
            &self.operation.idempotency_key,
            &self.operation.task_kind,
            &self.operation.agent_id,
            &self.operation.effect_scope,
            &self.operation.input_digest,
        )?;
        if canonical.operation_digest != self.operation.operation_digest {
            return Err(EvaError::conflict(
                "effect operation digest does not match its immutable identity",
            ));
        }
        RequestId::parse(&self.prepared_task_id)?;
        if self.prepared_owner_generation == WriterGeneration::ZERO || self.prepared_attempt == 0 {
            return Err(EvaError::conflict(
                "effect prepared task fence is incomplete",
            ));
        }
        validate_sha256_digest("prepared_fence_digest", &self.prepared_fence_digest)?;
        match self.state {
            EffectLedgerState::Prepared => {
                if self.committed_at_ms.is_some()
                    || self.result_digest.is_some()
                    || self.result_size_bytes.is_some()
                {
                    return Err(EvaError::conflict(
                        "prepared effect record contains committed result fields",
                    ));
                }
            }
            EffectLedgerState::Committed => {
                let committed_at_ms = self.committed_at_ms.ok_or_else(|| {
                    EvaError::conflict("committed effect record is missing commit time")
                })?;
                if committed_at_ms < self.prepared_at_ms {
                    return Err(EvaError::conflict(
                        "effect commit time precedes its prepare boundary",
                    ));
                }
                validate_sha256_digest(
                    "result_digest",
                    self.result_digest.as_deref().ok_or_else(|| {
                        EvaError::conflict("committed effect record is missing result digest")
                    })?,
                )?;
                if self.result_size_bytes.is_none() {
                    return Err(EvaError::conflict(
                        "committed effect record is missing result size",
                    ));
                }
            }
        }
        Ok(())
    }
}

impl EffectPrepareOutcome {
    /// Record observed after the serialized prepare decision.
    pub fn record(&self) -> &EffectLedgerRecord {
        match self {
            Self::Created(record) | Self::Prepared(record) | Self::Committed(record) => record,
        }
    }
}

impl FileSystemEffectLedger {
    /// Opens the standard effect directory under one active runtime writer.
    pub fn open_with_writer(
        layout: &DurableBackendLayout,
        writer: DurableWriterGuard,
    ) -> Result<Self, EvaError> {
        if writer.root() != layout.root {
            return Err(EvaError::conflict(
                "effect ledger writer belongs to a different backend root",
            )
            .with_context("layout_root", layout.root.display().to_string())
            .with_context("writer_root", writer.root().display().to_string()));
        }
        let root = layout.state_dir.join(EFFECT_LEDGER_DIR);
        let records = writer.with_write_lock(|_| {
            if root.exists() {
                if !root.is_dir() {
                    return Err(EvaError::conflict("effect ledger path is not a directory")
                        .with_context("path", root.display().to_string()));
                }
            } else {
                fs::create_dir_all(&root).map_err(|error| {
                    EvaError::internal("failed to create effect ledger directory")
                        .with_context("path", root.display().to_string())
                        .with_context("io_error", error.to_string())
                })?;
            }
            load_effect_records(&root)
        })?;
        Ok(Self {
            root,
            records,
            writer: Some(writer),
        })
    }

    /// Opens a validated read-only snapshot; a missing directory is an empty ledger.
    pub fn open_read_only(layout: &DurableBackendLayout) -> Result<Self, EvaError> {
        let root = layout.state_dir.join(EFFECT_LEDGER_DIR);
        let records = if root.exists() {
            if !root.is_dir() {
                return Err(EvaError::conflict("effect ledger path is not a directory")
                    .with_context("path", root.display().to_string()));
            }
            load_effect_records(&root)?
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            root,
            records,
            writer: None,
        })
    }

    /// Root containing hashed `.effect` records.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Number of fully validated records in the current view.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the current view contains no records.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Refreshes and returns the record for an exact business identity.
    ///
    /// Reusing one idempotency key for a different kind, Agent, or input fails closed.
    pub fn inspect(
        &mut self,
        operation: &EffectOperationIdentity,
    ) -> Result<Option<EffectLedgerRecord>, EvaError> {
        self.refresh()?;
        match self.records.get(operation.idempotency_key()) {
            Some(record) => {
                ensure_operation_matches(record, operation)?;
                Ok(Some(record.clone()))
            }
            None => Ok(None),
        }
    }

    /// Inspects an existing operation while serializing against cancellation of the exact claim.
    ///
    /// `None` means the caller may validate input and then call `prepare_for_claim`; it is not an
    /// execution permit because another task may establish the same operation in between.
    pub fn inspect_for_claim(
        &mut self,
        task_store: &FileSystemTaskStateStore,
        intent: &EffectLedgerIntent,
    ) -> Result<Option<EffectLedgerRecord>, EvaError> {
        self.with_claim_transaction(task_store, intent, |_root, records, _observed_at_ms| {
            match records.get(intent.operation.idempotency_key()) {
                Some(record) => {
                    ensure_operation_matches(record, &intent.operation)?;
                    Ok(Some(record.clone()))
                }
                None => Ok(None),
            }
        })
    }

    /// Atomically establishes prepare only while the exact task attempt is still runnable.
    pub fn prepare_for_claim(
        &mut self,
        task_store: &FileSystemTaskStateStore,
        intent: &EffectLedgerIntent,
    ) -> Result<EffectPrepareOutcome, EvaError> {
        self.with_claim_transaction(task_store, intent, |root, records, observed_at_ms| {
            let mut linearized_intent = intent.clone();
            linearized_intent.prepared_at_ms = observed_at_ms;
            prepare_effect_record(root, records, &linearized_intent)
        })
    }

    /// Low-level state-machine helper used by storage tests.
    #[cfg(test)]
    fn prepare(&mut self, intent: &EffectLedgerIntent) -> Result<EffectPrepareOutcome, EvaError> {
        self.with_write_transaction(|root, records| prepare_effect_record(root, records, intent))
    }

    /// Advances a prepared record to committed under the exact preparing task fence.
    #[allow(clippy::too_many_arguments)]
    pub fn commit(
        &mut self,
        intent: &EffectLedgerIntent,
        result_digest: &str,
        result_size_bytes: usize,
        committed_at_ms: u128,
    ) -> Result<EffectLedgerRecord, EvaError> {
        validate_sha256_digest("result_digest", result_digest)?;
        if self.writer.as_ref().map(DurableWriterGuard::generation) != Some(intent.owner_generation)
        {
            return Err(
                EvaError::conflict("effect commit belongs to another writer generation")
                    .with_context("operation_digest", intent.operation.operation_digest()),
            );
        }
        self.with_write_transaction(|root, records| {
            let current = records
                .get(intent.operation.idempotency_key())
                .cloned()
                .ok_or_else(|| {
                    EvaError::conflict("effect cannot commit before prepare")
                        .with_context("operation_digest", intent.operation.operation_digest())
                })?;
            ensure_operation_matches(&current, &intent.operation)?;
            if current.prepared_task_id != intent.task_id
                || current.prepared_owner_generation != intent.owner_generation
                || current.prepared_attempt != intent.attempt
                || current.prepared_fence_digest != intent.fence_digest
            {
                return Err(EvaError::conflict(
                    "effect commit task fence does not match the prepare owner",
                )
                .with_context("operation_digest", intent.operation.operation_digest()));
            }
            if current.state == EffectLedgerState::Committed {
                if current.result_digest.as_deref() == Some(result_digest)
                    && current.result_size_bytes == Some(result_size_bytes)
                {
                    return Ok(current);
                }
                return Err(EvaError::conflict(
                    "committed effect result identity cannot be rewritten",
                )
                .with_context("operation_digest", intent.operation.operation_digest()));
            }
            let mut committed = current;
            committed.state = EffectLedgerState::Committed;
            committed.committed_at_ms = Some(committed_at_ms);
            committed.result_digest = Some(result_digest.to_owned());
            committed.result_size_bytes = Some(result_size_bytes);
            committed.validate()?;
            persist_effect_record(root, &committed)?;
            records.insert(
                intent.operation.idempotency_key().to_owned(),
                committed.clone(),
            );
            Ok(committed)
        })
    }

    fn refresh(&mut self) -> Result<(), EvaError> {
        if let Some(writer) = self.writer.clone() {
            let root = self.root.clone();
            let records = writer.with_write_lock(|_| load_effect_records(&root))?;
            self.records = records;
            return Ok(());
        }
        self.records = if self.root.exists() {
            load_effect_records(&self.root)?
        } else {
            BTreeMap::new()
        };
        Ok(())
    }

    fn with_write_transaction<T>(
        &mut self,
        operation: impl FnOnce(&Path, &mut BTreeMap<String, EffectLedgerRecord>) -> Result<T, EvaError>,
    ) -> Result<T, EvaError> {
        let writer = self
            .writer
            .clone()
            .ok_or_else(|| EvaError::conflict("read-only effect ledger cannot mutate records"))?;
        let root = self.root.clone();
        let (result, records) = writer.with_write_lock(|_| {
            let mut records = load_effect_records(&root)?;
            let result = operation(&root, &mut records)?;
            Ok((result, records))
        })?;
        self.records = records;
        Ok(result)
    }

    fn with_claim_transaction<T>(
        &mut self,
        task_store: &FileSystemTaskStateStore,
        intent: &EffectLedgerIntent,
        operation: impl FnOnce(
            &Path,
            &mut BTreeMap<String, EffectLedgerRecord>,
            u128,
        ) -> Result<T, EvaError>,
    ) -> Result<T, EvaError> {
        let writer = self
            .writer
            .clone()
            .ok_or_else(|| EvaError::conflict("read-only effect ledger cannot prepare records"))?;
        if task_store.project_root() != writer.root()
            || task_store.runtime_writer_generation() != Some(writer.generation())
        {
            return Err(EvaError::conflict(
                "effect ledger and task store do not share runtime writer ownership",
            ));
        }
        let root = self.root.clone();
        let (result, records) = writer.with_write_lock(|generation| {
            let observed_at_ms = effect_now_ms()?;
            verify_effect_task_claim(task_store, intent, generation, observed_at_ms)?;
            let mut records = load_effect_records(&root)?;
            let result = operation(&root, &mut records, observed_at_ms)?;
            Ok((result, records))
        })?;
        self.records = records;
        Ok(result)
    }
}

impl fmt::Debug for EffectOperationIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectOperationIdentity")
            .field("idempotency_key", &"<redacted>")
            .field("operation_digest", &self.operation_digest)
            .field("task_kind", &self.task_kind)
            .field("agent_id", &self.agent_id)
            .field("effect_scope", &self.effect_scope)
            .field("input_digest", &self.input_digest)
            .finish()
    }
}

impl fmt::Debug for EffectLedgerIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectLedgerIntent")
            .field("operation", &self.operation)
            .field("task_id", &self.task_id)
            .field("owner_generation", &self.owner_generation)
            .field("attempt", &self.attempt)
            .field("fence_digest", &"<redacted>")
            .field("prepared_at_ms", &self.prepared_at_ms)
            .finish()
    }
}

impl fmt::Debug for EffectLedgerRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectLedgerRecord")
            .field("operation", &self.operation)
            .field("state", &self.state)
            .field("prepared_task_id", &self.prepared_task_id)
            .field("prepared_owner_generation", &self.prepared_owner_generation)
            .field("prepared_attempt", &self.prepared_attempt)
            .field("prepared_fence_digest", &"<redacted>")
            .field("prepared_at_ms", &self.prepared_at_ms)
            .field("committed_at_ms", &self.committed_at_ms)
            .field("result_digest", &self.result_digest)
            .field("result_size_bytes", &self.result_size_bytes)
            .finish()
    }
}

impl fmt::Debug for FileSystemEffectLedger {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileSystemEffectLedger")
            .field("root", &self.root)
            .field("record_count", &self.records.len())
            .field("writer_bound", &self.writer.is_some())
            .finish()
    }
}

fn ensure_operation_matches(
    record: &EffectLedgerRecord,
    operation: &EffectOperationIdentity,
) -> Result<(), EvaError> {
    if &record.operation == operation {
        return Ok(());
    }
    Err(
        EvaError::conflict("effect idempotency key is already bound to another operation")
            .with_context(
                "existing_operation_digest",
                record.operation.operation_digest(),
            )
            .with_context("requested_operation_digest", operation.operation_digest()),
    )
}

fn prepare_effect_record(
    root: &Path,
    records: &mut BTreeMap<String, EffectLedgerRecord>,
    intent: &EffectLedgerIntent,
) -> Result<EffectPrepareOutcome, EvaError> {
    if let Some(record) = records.get(intent.operation.idempotency_key()) {
        ensure_operation_matches(record, &intent.operation)?;
        return Ok(match record.state {
            EffectLedgerState::Prepared => EffectPrepareOutcome::Prepared(record.clone()),
            EffectLedgerState::Committed => EffectPrepareOutcome::Committed(record.clone()),
        });
    }
    let record = EffectLedgerRecord::prepared(intent);
    record.validate()?;
    persist_effect_record(root, &record)?;
    records.insert(
        intent.operation.idempotency_key().to_owned(),
        record.clone(),
    );
    Ok(EffectPrepareOutcome::Created(record))
}

fn verify_effect_task_claim(
    task_store: &FileSystemTaskStateStore,
    intent: &EffectLedgerIntent,
    writer_generation: WriterGeneration,
    observed_at_ms: u128,
) -> Result<(), EvaError> {
    if intent.owner_generation != writer_generation {
        return Err(
            EvaError::conflict("effect prepare belongs to another writer generation")
                .with_context("task_id", &intent.task_id),
        );
    }
    let snapshot = task_store.read(Some(&intent.task_id))?;
    if snapshot.status != "running" || snapshot.cancel_requested || snapshot.cancel_accepted {
        return Err(
            EvaError::conflict("effect prepare requires an uncancelled running task")
                .with_retryable(false)
                .with_context("task_id", &intent.task_id)
                .with_context("status", &snapshot.status),
        );
    }
    if snapshot.deadline_expired(observed_at_ms) {
        return Err(EvaError::timeout(
            "effect task deadline expired before prepare",
        ));
    }
    let envelope = snapshot.envelope.as_ref().ok_or_else(|| {
        EvaError::conflict("effect task is missing its immutable envelope")
            .with_context("task_id", &intent.task_id)
    })?;
    if envelope.idempotency_key != intent.operation.idempotency_key
        || envelope.kind != intent.operation.task_kind
        || envelope.agent_id != intent.operation.agent_id
        || envelope.input.digest() != intent.operation.input_digest
    {
        return Err(EvaError::conflict(
            "effect operation does not match the claimed task envelope",
        )
        .with_context("task_id", &intent.task_id)
        .with_context("operation_digest", intent.operation.operation_digest()));
    }
    let execution_owner = snapshot.execution_owner.as_deref().ok_or_else(|| {
        EvaError::conflict("effect task is missing its execution owner")
            .with_context("task_id", &intent.task_id)
    })?;
    let cancel_token = snapshot.cancel_token.as_deref().ok_or_else(|| {
        EvaError::conflict("effect task is missing its cancellation fence")
            .with_context("task_id", &intent.task_id)
    })?;
    let generation = snapshot.owner_generation.0.to_string();
    let attempt = snapshot.attempts.to_string();
    let actual_fence_digest = canonical_digest(
        "eva.effect-attempt-fence.v1",
        &[
            &snapshot.task_id,
            &generation,
            execution_owner,
            &attempt,
            cancel_token,
        ],
    );
    if snapshot.owner_generation != intent.owner_generation
        || snapshot.attempts != intent.attempt
        || actual_fence_digest != intent.fence_digest
    {
        return Err(
            EvaError::conflict("effect prepare task fence does not match the live claim")
                .with_context("task_id", &intent.task_id),
        );
    }
    Ok(())
}

fn effect_now_ms() -> Result<u128, EvaError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|error| {
            EvaError::internal("system clock is before the Unix epoch")
                .with_context("clock_error", error.to_string())
        })
}

fn load_effect_records(root: &Path) -> Result<BTreeMap<String, EffectLedgerRecord>, EvaError> {
    let mut records = BTreeMap::new();
    for entry in fs::read_dir(root).map_err(|error| {
        EvaError::internal("failed to read effect ledger directory")
            .with_context("path", root.display().to_string())
            .with_context("io_error", error.to_string())
    })? {
        let entry = entry.map_err(|error| {
            EvaError::internal("failed to read effect ledger entry")
                .with_context("path", root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some(EFFECT_RECORD_EXTENSION) {
            continue;
        }
        if !entry
            .file_type()
            .map_err(|error| {
                EvaError::conflict("failed to inspect effect ledger entry")
                    .with_context("path", path.display().to_string())
                    .with_context("io_error", error.to_string())
            })?
            .is_file()
        {
            return Err(
                EvaError::conflict("effect ledger entry is not a regular file")
                    .with_context("path", path.display().to_string()),
            );
        }
        let data = fs::read_to_string(&path).map_err(|error| {
            EvaError::conflict("failed to read effect ledger record")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let record = effect_record_from_storage(&data)
            .map_err(|error| error.with_context("path", path.display().to_string()))?;
        let expected_stem = effect_record_stem(record.operation.idempotency_key());
        if path.file_stem().and_then(|value| value.to_str()) != Some(expected_stem.as_str()) {
            return Err(
                EvaError::conflict("effect ledger file key does not match record")
                    .with_context("path", path.display().to_string()),
            );
        }
        if records
            .insert(record.operation.idempotency_key().to_owned(), record)
            .is_some()
        {
            return Err(EvaError::conflict(
                "effect ledger idempotency key is duplicated",
            ));
        }
    }
    Ok(records)
}

fn persist_effect_record(root: &Path, record: &EffectLedgerRecord) -> Result<(), EvaError> {
    let path = root.join(format!(
        "{}.{}",
        effect_record_stem(record.operation.idempotency_key()),
        EFFECT_RECORD_EXTENSION
    ));
    atomic_write(&path, effect_record_to_storage(record).as_bytes()).map_err(|error| {
        EvaError::internal("failed to atomically write effect ledger record")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })
}

fn effect_record_stem(idempotency_key: &str) -> String {
    canonical_digest("eva.effect-record-key.v1", &[idempotency_key])
        .trim_start_matches("sha256:")
        .to_owned()
}

fn effect_record_to_storage(record: &EffectLedgerRecord) -> String {
    format!(
        concat!(
            "format={}\n",
            "idempotency_key={}\n",
            "operation_digest={}\n",
            "task_kind={}\n",
            "agent_id={}\n",
            "effect_scope={}\n",
            "input_digest={}\n",
            "state={}\n",
            "prepared_task_id={}\n",
            "prepared_owner_generation={}\n",
            "prepared_attempt={}\n",
            "prepared_fence_digest={}\n",
            "prepared_at_ms={}\n",
            "committed_at_ms={}\n",
            "result_digest={}\n",
            "result_size_bytes={}\n"
        ),
        EFFECT_LEDGER_FORMAT,
        record.operation.idempotency_key,
        record.operation.operation_digest,
        record.operation.task_kind,
        record.operation.agent_id,
        record.operation.effect_scope,
        record.operation.input_digest,
        record.state.as_str(),
        record.prepared_task_id,
        record.prepared_owner_generation.0,
        record.prepared_attempt,
        record.prepared_fence_digest,
        record.prepared_at_ms,
        record
            .committed_at_ms
            .map(|value| value.to_string())
            .unwrap_or_default(),
        record.result_digest.as_deref().unwrap_or_default(),
        record
            .result_size_bytes
            .map(|value| value.to_string())
            .unwrap_or_default()
    )
}

fn effect_record_from_storage(data: &str) -> Result<EffectLedgerRecord, EvaError> {
    let mut fields = BTreeMap::new();
    for line in data.lines().filter(|line| !line.is_empty()) {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| EvaError::conflict("effect ledger record field is invalid"))?;
        if !matches!(
            key,
            "format"
                | "idempotency_key"
                | "operation_digest"
                | "task_kind"
                | "agent_id"
                | "effect_scope"
                | "input_digest"
                | "state"
                | "prepared_task_id"
                | "prepared_owner_generation"
                | "prepared_attempt"
                | "prepared_fence_digest"
                | "prepared_at_ms"
                | "committed_at_ms"
                | "result_digest"
                | "result_size_bytes"
        ) {
            return Err(EvaError::conflict("effect ledger record has unknown field")
                .with_context("field", key));
        }
        if fields.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(
                EvaError::conflict("effect ledger record field is duplicated")
                    .with_context("field", key),
            );
        }
    }
    if fields.len() != EFFECT_RECORD_FIELD_COUNT {
        return Err(EvaError::conflict(
            "effect ledger record field set is incomplete",
        ));
    }
    if required_field(&fields, "format")? != EFFECT_LEDGER_FORMAT {
        return Err(EvaError::conflict(
            "effect ledger record format is unsupported",
        ));
    }
    let idempotency_key = required_field(&fields, "idempotency_key")?;
    let task_kind = required_field(&fields, "task_kind")?;
    let agent_id = required_field(&fields, "agent_id")?;
    let effect_scope = required_field(&fields, "effect_scope")?;
    let input_digest = required_field(&fields, "input_digest")?;
    let operation = EffectOperationIdentity::new(
        idempotency_key,
        task_kind,
        agent_id,
        effect_scope,
        input_digest,
    )?;
    if operation.operation_digest() != required_field(&fields, "operation_digest")? {
        return Err(EvaError::conflict(
            "effect operation digest does not match stored identity",
        ));
    }
    let record = EffectLedgerRecord {
        operation,
        state: EffectLedgerState::from_storage(required_field(&fields, "state")?)?,
        prepared_task_id: required_field(&fields, "prepared_task_id")?.to_owned(),
        prepared_owner_generation: WriterGeneration(parse_field(
            &fields,
            "prepared_owner_generation",
        )?),
        prepared_attempt: parse_field(&fields, "prepared_attempt")?,
        prepared_fence_digest: required_field(&fields, "prepared_fence_digest")?.to_owned(),
        prepared_at_ms: parse_field(&fields, "prepared_at_ms")?,
        committed_at_ms: parse_optional_field(&fields, "committed_at_ms")?,
        result_digest: optional_text_field(&fields, "result_digest")?,
        result_size_bytes: parse_optional_field(&fields, "result_size_bytes")?,
    };
    record.validate()?;
    Ok(record)
}

fn required_field<'a>(
    fields: &'a BTreeMap<String, String>,
    key: &str,
) -> Result<&'a str, EvaError> {
    fields.get(key).map(String::as_str).ok_or_else(|| {
        EvaError::conflict("effect ledger record field is missing").with_context("field", key)
    })
}

fn optional_text_field(
    fields: &BTreeMap<String, String>,
    key: &str,
) -> Result<Option<String>, EvaError> {
    let value = required_field(fields, key)?;
    Ok((!value.is_empty()).then(|| value.to_owned()))
}

fn parse_field<T>(fields: &BTreeMap<String, String>, key: &str) -> Result<T, EvaError>
where
    T: std::str::FromStr,
{
    required_field(fields, key)?.parse::<T>().map_err(|_| {
        EvaError::conflict("effect ledger numeric field is invalid").with_context("field", key)
    })
}

fn parse_optional_field<T>(
    fields: &BTreeMap<String, String>,
    key: &str,
) -> Result<Option<T>, EvaError>
where
    T: std::str::FromStr,
{
    let value = required_field(fields, key)?;
    if value.is_empty() {
        return Ok(None);
    }
    value.parse::<T>().map(Some).map_err(|_| {
        EvaError::conflict("effect ledger numeric field is invalid").with_context("field", key)
    })
}

fn validate_fence_field(field: &str, value: &str) -> Result<(), EvaError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > MAX_EFFECT_FENCE_FIELD_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(
            EvaError::invalid_argument("effect task fence field is invalid")
                .with_context("field", field),
        );
    }
    Ok(())
}

fn validate_sha256_digest(field: &str, value: &str) -> Result<(), EvaError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(EvaError::conflict("effect digest is not canonical SHA-256")
            .with_context("field", field));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(EvaError::conflict("effect digest is not canonical SHA-256")
            .with_context("field", field));
    }
    Ok(())
}

fn canonical_digest(domain: &str, fields: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update((domain.len() as u64).to_be_bytes());
    hasher.update(domain.as_bytes());
    for field in fields {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DurableBackendOptions, FileSystemDurableBackend, TaskAttemptPolicySnapshot,
        TaskEnvelopeSnapshot, TaskStateSnapshot,
    };
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root(name: &str) -> TestRoot {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "eva-effect-ledger-{name}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        TestRoot(path)
    }

    fn operation(key: &str, input: &[u8]) -> EffectOperationIdentity {
        EffectOperationIdentity::new(
            key,
            "vendor.non-idempotent",
            "root-agent",
            "vendor.non-idempotent.v1",
            canonical_digest("test.input.v1", &[std::str::from_utf8(input).unwrap()]),
        )
        .unwrap()
    }

    fn intent(
        operation: EffectOperationIdentity,
        generation: WriterGeneration,
        task_id: &str,
        attempt: usize,
    ) -> EffectLedgerIntent {
        EffectLedgerIntent::new(
            operation,
            task_id,
            generation,
            "daemon:test:worker",
            attempt,
            &format!("cancel:{task_id}:{attempt}"),
            100,
        )
        .unwrap()
    }

    fn only_record_path(ledger: &FileSystemEffectLedger) -> PathBuf {
        fs::read_dir(ledger.root())
            .unwrap()
            .find_map(|entry| {
                let path = entry.unwrap().path();
                (path.extension().and_then(|value| value.to_str()) == Some(EFFECT_RECORD_EXTENSION))
                    .then_some(path)
            })
            .unwrap()
    }

    #[test]
    fn prepared_effect_commits_once_and_reopens_without_result_bytes() {
        let root = test_root("commit-reopen");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let operation = operation("idem-effect-commit", b"payload");
        let intent = intent(
            operation.clone(),
            writer.generation(),
            "req-effect-commit",
            1,
        );
        assert!(matches!(
            ledger.prepare(&intent).unwrap(),
            EffectPrepareOutcome::Created(_)
        ));
        let result_digest = canonical_digest("test.result.v1", &["result-bytes"]);
        let committed = ledger.commit(&intent, &result_digest, 12, 120).unwrap();
        assert_eq!(committed.state(), EffectLedgerState::Committed);
        assert_eq!(committed.result_digest(), Some(result_digest.as_str()));
        assert_eq!(committed.result_size_bytes(), Some(12));
        assert_eq!(
            ledger.commit(&intent, &result_digest, 12, 130).unwrap(),
            committed
        );
        drop(ledger);
        drop(writer);

        let reopened = FileSystemEffectLedger::open_read_only(backend.layout()).unwrap();
        assert_eq!(reopened.len(), 1);
        let disk = fs::read_to_string(
            fs::read_dir(reopened.root())
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path(),
        )
        .unwrap();
        assert!(disk.contains("state=committed\n"));
        assert!(disk.contains(&format!("result_digest={result_digest}\n")));
        assert!(!disk.contains("result-bytes"));
    }

    #[test]
    fn idempotency_key_collision_and_commit_fence_mismatch_fail_closed() {
        let root = test_root("collision-fence");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let first_operation = operation("idem-effect-collision", b"first");
        let first = intent(
            first_operation.clone(),
            writer.generation(),
            "req-effect-first",
            1,
        );
        ledger.prepare(&first).unwrap();

        let collision = operation("idem-effect-collision", b"second");
        let error = ledger.inspect(&collision).unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        let competing = intent(
            first_operation,
            writer.generation(),
            "req-effect-competing",
            1,
        );
        let digest = canonical_digest("test.result.v1", &["result"]);
        let error = ledger.commit(&competing, &digest, 6, 120).unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    fn committed_result_identity_is_monotonic() {
        let root = test_root("result-monotonic");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let intent = intent(
            operation("idem-effect-monotonic", b"payload"),
            writer.generation(),
            "req-effect-monotonic",
            1,
        );
        ledger.prepare(&intent).unwrap();
        let first = canonical_digest("test.result.v1", &["first"]);
        ledger.commit(&intent, &first, 5, 110).unwrap();
        let second = canonical_digest("test.result.v1", &["second"]);
        let error = ledger.commit(&intent, &second, 6, 120).unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        let record = ledger.inspect(intent.operation()).unwrap().unwrap();
        assert_eq!(record.result_digest(), Some(first.as_str()));
        assert_eq!(record.result_size_bytes(), Some(5));
    }

    #[test]
    fn competing_writer_clones_prepare_one_effect_once() {
        let root = test_root("concurrent-prepare");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let first_ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let second_ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let operation = operation("idem-effect-concurrent", b"payload");
        let first_intent = intent(
            operation.clone(),
            writer.generation(),
            "req-effect-concurrent-a",
            1,
        );
        let second_intent = intent(operation, writer.generation(), "req-effect-concurrent-b", 1);
        let barrier = Arc::new(Barrier::new(3));
        let first_barrier = Arc::clone(&barrier);
        let first = thread::spawn(move || {
            let mut ledger = first_ledger;
            first_barrier.wait();
            ledger.prepare(&first_intent).unwrap()
        });
        let second_barrier = Arc::clone(&barrier);
        let second = thread::spawn(move || {
            let mut ledger = second_ledger;
            second_barrier.wait();
            ledger.prepare(&second_intent).unwrap()
        });
        barrier.wait();
        let outcomes = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, EffectPrepareOutcome::Created(_)))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, EffectPrepareOutcome::Prepared(_)))
                .count(),
            1
        );
        assert_eq!(
            FileSystemEffectLedger::open_read_only(backend.layout())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn prepare_for_claim_rejects_cancelled_or_expired_authority() {
        let root = test_root("claim-authority");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut store =
            FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
                .unwrap();
        let mut ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();

        let cancelled_envelope = TaskEnvelopeSnapshot::inline(
            "vendor.non-idempotent",
            "root-agent",
            b"cancelled-payload".to_vec(),
            "idem-effect-cancelled",
            TaskAttemptPolicySnapshot::new(1, 0, None).unwrap(),
        )
        .unwrap();
        store
            .create(
                &TaskStateSnapshot::queued_with_envelope(
                    "req-effect-cancelled",
                    cancelled_envelope.clone(),
                )
                .unwrap(),
            )
            .unwrap();
        let cancelled_claim = store
            .try_claim_queued(
                "req-effect-cancelled",
                "daemon:claim:cancelled",
                "cancel:claim:cancelled",
                100,
            )
            .unwrap()
            .unwrap();
        store
            .request_cancellation("req-effect-cancelled", "operator cancellation")
            .unwrap();
        let cancelled_operation = EffectOperationIdentity::new(
            "idem-effect-cancelled",
            "vendor.non-idempotent",
            "root-agent",
            "vendor.non-idempotent.v1",
            cancelled_envelope.input.digest(),
        )
        .unwrap();
        let cancelled_intent = EffectLedgerIntent::new(
            cancelled_operation.clone(),
            cancelled_claim.fence().task_id(),
            cancelled_claim.fence().owner_generation(),
            cancelled_claim.fence().execution_owner(),
            cancelled_claim.fence().attempt(),
            cancelled_claim.fence().cancel_token(),
            101,
        )
        .unwrap();
        let error = ledger
            .prepare_for_claim(&store, &cancelled_intent)
            .unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert!(!error.is_retryable());
        assert!(ledger.inspect(&cancelled_operation).unwrap().is_none());

        let expired_envelope = TaskEnvelopeSnapshot::inline(
            "vendor.non-idempotent",
            "root-agent",
            b"expired-payload".to_vec(),
            "idem-effect-expired",
            TaskAttemptPolicySnapshot::new(1, 0, Some(1)).unwrap(),
        )
        .unwrap();
        store
            .create(
                &TaskStateSnapshot::queued_with_envelope(
                    "req-effect-expired",
                    expired_envelope.clone(),
                )
                .unwrap(),
            )
            .unwrap();
        let expired_claim = store
            .try_claim_queued(
                "req-effect-expired",
                "daemon:claim:expired",
                "cancel:claim:expired",
                100,
            )
            .unwrap()
            .unwrap();
        let expired_operation = EffectOperationIdentity::new(
            "idem-effect-expired",
            "vendor.non-idempotent",
            "root-agent",
            "vendor.non-idempotent.v1",
            expired_envelope.input.digest(),
        )
        .unwrap();
        let expired_intent = EffectLedgerIntent::new(
            expired_operation.clone(),
            expired_claim.fence().task_id(),
            expired_claim.fence().owner_generation(),
            expired_claim.fence().execution_owner(),
            expired_claim.fence().attempt(),
            expired_claim.fence().cancel_token(),
            101,
        )
        .unwrap();
        let error = ledger
            .prepare_for_claim(&store, &expired_intent)
            .unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Timeout);
        assert!(ledger.inspect(&expired_operation).unwrap().is_none());
    }

    #[test]
    fn prepare_deadline_is_checked_after_waiting_for_writer_lock() {
        let root = test_root("claim-lock-deadline");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut store =
            FileSystemTaskStateStore::from_runtime_writer(backend.layout(), writer.clone())
                .unwrap();
        let envelope = TaskEnvelopeSnapshot::inline(
            "vendor.non-idempotent",
            "root-agent",
            b"lock-wait-payload".to_vec(),
            "idem-effect-lock-wait",
            TaskAttemptPolicySnapshot::new(1, 0, Some(25)).unwrap(),
        )
        .unwrap();
        store
            .create(
                &TaskStateSnapshot::queued_with_envelope("req-effect-lock-wait", envelope.clone())
                    .unwrap(),
            )
            .unwrap();
        let claimed_at_ms = effect_now_ms().unwrap();
        let claim = store
            .try_claim_queued(
                "req-effect-lock-wait",
                "daemon:claim:lock-wait",
                "cancel:claim:lock-wait",
                claimed_at_ms,
            )
            .unwrap()
            .unwrap();
        let operation = EffectOperationIdentity::new(
            "idem-effect-lock-wait",
            "vendor.non-idempotent",
            "root-agent",
            "vendor.non-idempotent.v1",
            envelope.input.digest(),
        )
        .unwrap();
        let intent = EffectLedgerIntent::new(
            operation,
            claim.fence().task_id(),
            claim.fence().owner_generation(),
            claim.fence().execution_owner(),
            claim.fence().attempt(),
            claim.fence().cancel_token(),
            claimed_at_ms,
        )
        .unwrap();
        let ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let prepare_barrier = Arc::clone(&barrier);
        let prepare_thread = writer
            .with_write_lock(|_| {
                let handle = thread::spawn(move || {
                    let mut ledger = ledger;
                    prepare_barrier.wait();
                    ledger.prepare_for_claim(&store, &intent)
                });
                barrier.wait();
                thread::sleep(std::time::Duration::from_millis(75));
                Ok(handle)
            })
            .unwrap();
        let error = prepare_thread.join().unwrap().unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Timeout);
        assert!(FileSystemEffectLedger::open_read_only(backend.layout())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn parser_rejects_field_shape_and_same_length_digest_tampering() {
        let root = test_root("strict-parser");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let intent = intent(
            operation("idem-effect-strict-parser", b"payload"),
            writer.generation(),
            "req-effect-strict-parser",
            1,
        );
        let record = ledger.prepare(&intent).unwrap().record().clone();
        let path = only_record_path(&ledger);
        let original = fs::read_to_string(&path).unwrap();
        let missing = original
            .lines()
            .filter(|line| !line.starts_with("effect_scope="))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let duplicate = format!("{original}state=prepared\n");
        let unknown = format!("{original}unexpected=value\n");
        let digest = record.operation().operation_digest();
        let replacement = if digest.ends_with('0') { '1' } else { '0' };
        let mut changed_digest = digest.to_owned();
        changed_digest.replace_range(changed_digest.len() - 1.., &replacement.to_string());
        let tampered = original.replacen(digest, &changed_digest, 1);

        for invalid in [missing, duplicate, unknown, tampered] {
            fs::write(&path, invalid).unwrap();
            let error = FileSystemEffectLedger::open_read_only(backend.layout()).unwrap_err();
            assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
            fs::write(&path, &original).unwrap();
        }

        fs::write(ledger.root().join("interrupted.effect.tmp"), b"partial").unwrap();
        assert_eq!(
            FileSystemEffectLedger::open_read_only(backend.layout())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn stale_writer_commit_and_read_only_mutation_fail_closed() {
        let root = test_root("stale-writer");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let old_generation = writer.generation();
        let effect_operation = operation("idem-effect-stale-writer", b"payload");
        let old_intent = intent(
            effect_operation.clone(),
            old_generation,
            "req-effect-stale-writer",
            1,
        );
        let mut ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();
        ledger.prepare(&old_intent).unwrap();
        drop(ledger);
        drop(writer);

        let writer = backend.acquire_runtime_writer().unwrap();
        assert!(writer.generation() > old_generation);
        let mut ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let result_digest = canonical_digest("test.result.v1", &["result"]);
        let error = ledger
            .commit(&old_intent, &result_digest, 6, 120)
            .unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
        assert_eq!(
            ledger.inspect(&effect_operation).unwrap().unwrap().state(),
            EffectLedgerState::Prepared
        );

        let mut read_only = FileSystemEffectLedger::open_read_only(backend.layout()).unwrap();
        let current_intent = intent(
            operation("idem-effect-read-only", b"payload"),
            writer.generation(),
            "req-effect-read-only",
            1,
        );
        let error = read_only.prepare(&current_intent).unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    fn corrupt_record_and_debug_output_fail_safe() {
        let root = test_root("corrupt-redaction");
        let backend =
            FileSystemDurableBackend::open(DurableBackendOptions::read_write(root.path())).unwrap();
        let writer = backend.acquire_runtime_writer().unwrap();
        let mut ledger =
            FileSystemEffectLedger::open_with_writer(backend.layout(), writer.clone()).unwrap();
        let secret_key = "idem-effect-secret-key";
        let secret_token = "cancel-secret-token";
        let operation = operation(secret_key, b"payload");
        let intent = EffectLedgerIntent::new(
            operation,
            "req-effect-redaction",
            writer.generation(),
            "daemon:secret-owner",
            1,
            secret_token,
            100,
        )
        .unwrap();
        let record = ledger.prepare(&intent).unwrap().record().clone();
        let debug = format!("{intent:?} {record:?} {ledger:?}");
        assert!(!debug.contains(secret_key));
        assert!(!debug.contains(secret_token));
        let record_path = fs::read_dir(ledger.root())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let corrupt = fs::read_to_string(&record_path)
            .unwrap()
            .replace("state=prepared", "state=unknown");
        fs::write(&record_path, corrupt).unwrap();
        drop(ledger);
        drop(writer);
        let error = FileSystemEffectLedger::open_read_only(backend.layout()).unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
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
}
