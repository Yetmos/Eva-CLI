# eva-storage / 持久化存储边界

更新时间：2026-07-16

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-storage` 负责 Eva-CLI 的状态、事件日志、task snapshot、audit record、artifact 和 provider process snapshot 存储契约。事件日志同时提供 in-memory 和 V1.6.2 filesystem durable 版本；task snapshot 已可使用 `.eva/tasks` 或 V1.6 durable backend 的 `tasks/` 布局，并在 V1.12.3 增加 heartbeat、deadline、cancel token、cancelling/recovering/interrupted 等长任务 lifecycle 字段；W1-L02 再以 `eva.task-state.v3` 保存不可变 TaskEnvelope、二进制 inline input 或 artifact ref、幂等键和 attempt policy；audit record 可写入 durable backend 的 `audit/` 目录；artifact 已提供 local filesystem backend；V1.13.5 新增 filesystem provider process table，用于记录受监督 provider execution slot 的跨进程 session/process recovery evidence。

## V0.4 当前实现

| 能力 | 类型/文件 | 当前行为 |
| --- | --- | --- |
| EventLog | `EventLog`、`InMemoryEventLog`、`FileSystemEventLog` | 支持 append、ack、fail、watermark、按 sequence replay。重复 event id 返回 `Conflict`。filesystem backend 将记录保存到 durable backend 的 `events/log`。 |
| EventLogRecord | `EventLogRecord`、`EventLogStatus` | 记录 sequence、原始 `Event`、消费 Agent、失败原因。 |
| StateStore | `StateStore`、`InMemoryStateStore`、`FileSystemStateStore` | 内存实现支持 get/put/CAS；文件实现要求 runtime writer ownership，在 OS lock 内持久化单调版本并原子替换。 |
| StateRecord | `StateRecord`、`StateVersion`、`WriterGeneration` | 保存 key、value、CAS version 和提交该版本的 writer generation。 |
| TaskStateStore | `TaskStateStore`、`FileSystemTaskStateStore` | 默认保存 `.eva/tasks` task snapshot，也可使用 durable backend 的 `tasks/` 目录；权威 ID 文件携带 record version/generation，既有任务必须 CAS，latest 是原子刷新但不与 ID 文件组成跨文件事务的派生别名。v3 TaskEnvelope 在 create 后不可被 lifecycle CAS 修改。 |
| AuditSink | `FileSystemAuditSink`、`AuditRecord` | 将 `AuditEvent` 写入 durable backend `audit/` 目录，保存 action/outcome/trace/message/fields，并支持按 trace id 查询。 |
| ArtifactStore | `ArtifactStore`、`ArtifactRecord`、`InMemoryArtifactStore`、`FileSystemArtifactStore` | 保存 bytes，并生成可重复 SHA-256 digest；filesystem backend 会落盘 bytes 和 v2 metadata，记录 size、content type、retention policy 和 retain-until timestamp，并在读取时重新校验 key、size 和 digest。 |
| ProviderProcessTable | `ProviderProcessTable`、`ProviderProcessSnapshot`、`InMemoryProviderProcessTable`、`FileSystemProviderProcessTable` | 记录 provider session、真实 PID/group-or-Job/start token、manifest digest、health、restart budget/attempt/due/state、record version、writer generation 和 audit；filesystem backend 以 fenced CAS 写入 `state/provider-processes/`，供 daemon restart recovery 扫描。 |
| SQLite | `sqlite.rs` | 仍是 durable backend 边界占位，V0.4 不引入 SQLite 依赖。 |

## 模块边界

`eva-storage` 只保存和返回数据，不解释 Agent 业务语义，不做 Topic 路由，不执行 Lua，也不决定 policy。调用方必须在写入前完成授权和 schema 判断。

`EventLog` 的 in-memory 实现只保证单进程测试闭环。`FileSystemEventLog` 提供跨进程重新打开后的查询和 replay 基线，但不负责 runtime crash recovery、调度重试或任务状态修复。未来 SQLite/WAL backend 接入时，应保持当前 trait 语义稳定：事件被 runtime 接受后先 append，处理完成后 ack 或 fail。

## 公开入口

```rust
use eva_storage::{
    DurableWriterGuard, EventLog, FileSystemEventLog, FileSystemStateStore,
    FileSystemTaskStateStore, InMemoryEventLog, FileSystemProviderProcessTable,
    InMemoryProviderProcessTable, InMemoryStateStore, ProviderProcessTable, StateStore,
    TaskAttemptPolicySnapshot, TaskEnvelopeSnapshot, TaskInputSnapshot,
};
```

主要 re-export 位于 `src/lib.rs`，下游 crate 不需要直接引用子模块路径。需要 durable event log 时使用 `FileSystemEventLog::open(backend.layout())`；需要兼容本地 task state 时使用 `FileSystemTaskStateStore::new(project_root)`；durable task/state 的只读视图使用 `from_durable_layout`，写入使用 `from_writable_backend(&backend)`，或先调用 `backend.acquire_runtime_writer()` 并把同一 guard clone 传给多个 store；需要 durable artifact evidence 时使用 `FileSystemArtifactStore::new(path)`；需要 provider supervisor process table 时使用 `InMemoryProviderProcessTable::new()`；需要 daemon restart recovery 可扫描的 provider process table 时使用 `FileSystemProviderProcessTable::from_durable_layout(backend.layout())`。

## 验证

当前模块验证命令：

```powershell
cargo test -p eva-storage
```

已覆盖：事件 append/watermark、ack consumer、fail structured error、replay cursor、filesystem EventLog 跨 reopen、真实双进程 runtime writer 竞争、generation 重开/损坏/耗尽、filesystem StateStore/TaskStateStore stale CAS、legacy task version 升级、v3 TaskEnvelope 跨重开/二进制 payload/摘要篡改/字段缺失/不可变 CAS、latest 修复、原子替换故障保旧值、snapshot 列表与 lifecycle、ArtifactStore digest/missing/tamper/legacy/corrupt checks，以及 ProviderProcessTable in-memory/filesystem upsert/list/release/restart interrupt。

## V1.6.1 Durable Backend Baseline

`FileSystemDurableBackend` defines the first durable backend layout contract:

- `backend.manifest` records `schema_version=1` and layout version `eva.durable.v1`.
- `events/`, `state/`, `tasks/`, `audit/`, and `artifacts/` are created and verified as stable directories.
- `migration.lock` is a stable OS-lock anchor held only while a read-write open initializes or repairs the layout; a successful backend handle does not retain it.
- read-only open verifies an existing backend without creating files or taking a lock.
- `InMemoryDurableBackend` remains available as the test backend.

## V1.6.2 Durable Event Log

`FileSystemEventLog` writes versioned key/value records under `events/log`.
It persists event id, topic, target, payload, metadata, delivery status,
consumer, and structured error fields. Reopening the same durable backend can
replay records by sequence and keeps the next sequence watermark monotonic.

## V1.6.3 Durable Task Store Adapter

`FileSystemTaskStateStore` now has three entry points:

- `new(project_root)` keeps the compatible `.eva/tasks` diagnostic path.
- `from_durable_layout(layout)` uses the V1.6 durable backend `tasks/`
  directory as a read-only restart view.
- `from_writable_backend(backend)` acquires fenced runtime writer ownership for
  versioned create/CAS mutations; `from_runtime_writer` shares one guard across stores.

The CLI exposes this through `--durable-backend <path>` on `run --example basic`
and `task status/logs/cancel`.

## W1-L01 Durable Writer Ownership and CAS

`FileSystemDurableBackend::acquire_runtime_writer()` locks the stable
`runtime.writer.lock` anchor with an OS advisory lock, then atomically advances
`runtime.writer.generation`. Guard clones share one process mutex, and the last
clone releases the OS lock automatically. `FileSystemStateStore` and durable
task mutations verify this generation before reading the current record version
and replacing the record.

Manifest, generation, state, task ID, and latest writes use a same-directory
create-new temp file, `write_all`, `flush`, `sync_all`, and atomic replace. Unix
also syncs the parent directory; Windows uses `MoveFileExW` with replace and
write-through flags and never removes the destination before replacement.

## W1-L02 Persisted TaskEnvelope

New daemon submissions use `eva.task-state.v3`. `TaskEnvelopeSnapshot` stores a
syntactically valid task kind and Agent ID, exactly one input source, a stable
idempotency key, and a fixed-width attempt policy. Inline input is limited to 1
MiB, encoded as lowercase hex, and rebound to its SHA-256 on every read;
artifact input stores a stable relative key and canonical lowercase
`sha256:<64 hex>` claim for the execution boundary to recheck.
Derived container Debug output remains usable for diagnosis, but inline bytes
are replaced by `<redacted>` plus their size and digest.

Legacy records without a format and `eva.task-state.v2` remain readable with
`envelope=None`; they are never given invented executable payloads. A v3
envelope is immutable after create, while status/log/cancel/recovery fields can
continue through fenced CAS. Valid but unregistered task kinds remain
persistable because handler lookup and stable unknown-handler failure belong to
W1-L05.

`FileSystemAuditSink::open(layout)` writes audit records under the same backend
`audit/` directory. `query_by_trace_id` can retrieve records by span, request,
event, correlation, or causation id.

## V1.16.4 Audit Retention Policy

`FileSystemAuditSink::open_with_policy(layout, policy, now_ms)` opens the same
durable backend `audit/` directory with an explicit
`ObservabilityRetentionPolicy::durable_audit()` policy. The loader can skip and
report corrupt `.audit` records, or fail fast, and deletes only records whose
recorded timestamp is outside the retention window. The next write sequence
still scans every `.audit` file name, including skipped corrupt records, so a
new audit event cannot overwrite evidence left behind for investigation.

`FileSystemArtifactStore` writes versioned metadata next to artifact bytes. V2
metadata includes the key, digest, size, content type, retention policy, and
optional retain-until timestamp. Reads continue to accept legacy metadata
without a version field, then normalize missing content type and retention
fields to defaults while still returning stable conflicts for corrupt metadata.

## V1.13.4 Provider Stream Artifacts

Provider stream capture remains owned by `eva-adapter`, while
`FileSystemArtifactStore` is the controlled sink for redacted bounded stream
bytes. The store continues to enforce stable relative keys, SHA-256 digest
metadata, size checks, content type, and retention metadata for stdout/stderr,
HTTP body, Skill output, and MCP result artifacts.

## V1.12.3 Durable Task Lifecycle

`TaskStateSnapshot` now carries the durable lifecycle fields needed by daemon
long-running tasks:

- stable states: `queued`、`running`、`cancelling`、`timed_out`、`completed`、`failed`、`interrupted`、`recovering`；
- heartbeat/deadline fields: `heartbeat_at_ms`、`deadline_at_ms`；
- cancellation fields: `cancel_requested`、`cancel_accepted`、`cancel_reason`、`cancel_token`；
- recovery field: `interrupted_reason`；
- append-only task logs through `push_log()` and `FileSystemTaskStateStore::update_snapshot()`.

Older task snapshot files remain readable because missing lifecycle fields
default to `None`. `daemon submit` creates a queued lifecycle snapshot, and
`daemon cancel` moves non-terminal tasks to `cancelling` rather than pretending
the long-running work has already stopped.

## V1.13.1 Provider Process Table

`ProviderProcessSnapshot` records the process/session boundary used by the
provider supervisor baseline:

- adapter/capability/request identifiers;
- provider session id and process id;
- manifest digest, start command, health, restart policy, active flag, and last error;
- append-only audit entries for acquire, completed, and failed release.

`InMemoryProviderProcessTable` is intentionally process-local for V1.13.1. It
supports querying active sessions by adapter and gives `eva-adapter` a stable
contract before later persistent recovery work.

## W3-L06 Provider Process Restart Store

`FileSystemProviderProcessTable::from_durable_layout(layout)` provides the
read-only restart view under `state/provider-processes/`; mutations require
`from_runtime_writer(layout, writer)`. Provider-process v3 preserves real OS
identity, monotonic process/restart attempts, immutable restart budget, absolute
restart due time, restart state, record version, and writer generation. Every
transition is fenced by the same runtime-writer and per-record CAS contract.
Legacy v1/v2 records remain readable and are upgraded exactly once by a current
writer. Pending/starting records are recoverable across daemon generations,
while stable, exhausted, and non-retryable terminal states fail closed.

## 后续计划

| 版本 | 计划 |
| --- | --- |
| V0.5 | 为 dead-letter 和任务日志增加查询索引。 |
| W3-L06 | provider process v3 已持久化 restart attempt/budget/due/state，并支持 daemon generation 变化后的 fenced recovery。 |
| V1.13.5 | 已新增 filesystem provider process table，daemon recovery 可扫描并中断残留 active session。 |
| V1.13.1 | 已新增 provider process table baseline，覆盖 supervisor slot acquire/release evidence。 |
| V1.12.3 | 已将 task snapshot 升级为 durable task lifecycle，覆盖 heartbeat、deadline、cancel token 和 task log append。 |
| V1.6.3 | 已将 task snapshot、audit record 和 artifact metadata hardening 接入 durable backend 相关布局。 |
| V1.6.2 | 已新增 filesystem durable event log，供 `eva-eventbus::DurableEventBus` 使用。 |
| V1.2 | 接入 memory/knowledge 的持久化状态模型。 |
| V1.4 | 将 migration、snapshot、release artifact 命令接入 filesystem durable backend。 |
