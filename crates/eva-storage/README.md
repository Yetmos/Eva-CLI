# eva-storage / 持久化存储边界

更新时间：2026-07-07

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-storage` 负责 Eva-CLI 的状态、事件日志、task snapshot、audit record 和 artifact 存储契约。事件日志同时提供 in-memory 和 V1.6.2 filesystem durable 版本；task snapshot 已可使用 `.eva/tasks` 或 V1.6 durable backend 的 `tasks/` 布局；audit record 可写入 durable backend 的 `audit/` 目录；artifact 已提供 local filesystem backend，用于跨进程 CLI 查询、备份、发布和后续 apply evidence 的持久化边界。

## V0.4 当前实现

| 能力 | 类型/文件 | 当前行为 |
| --- | --- | --- |
| EventLog | `EventLog`、`InMemoryEventLog`、`FileSystemEventLog` | 支持 append、ack、fail、watermark、按 sequence replay。重复 event id 返回 `Conflict`。filesystem backend 将记录保存到 durable backend 的 `events/log`。 |
| EventLogRecord | `EventLogRecord`、`EventLogStatus` | 记录 sequence、原始 `Event`、消费 Agent、失败原因。 |
| StateStore | `StateStore`、`InMemoryStateStore` | 支持 get、put、compare-and-set；版本从 1 单调递增。 |
| StateRecord | `StateRecord`、`StateVersion` | 保存 key、value 和 CAS version。 |
| TaskStateStore | `TaskStateStore`、`FileSystemTaskStateStore` | 默认保存 `.eva/tasks` task snapshot，也可通过 `DurableBackendLayout` 使用 durable backend 的 `tasks/` 目录；支持按 task id、latest 或 snapshot 列表跨进程读取。 |
| AuditSink | `FileSystemAuditSink`、`AuditRecord` | 将 `AuditEvent` 写入 durable backend `audit/` 目录，保存 action/outcome/trace/message/fields，并支持按 trace id 查询。 |
| ArtifactStore | `ArtifactStore`、`ArtifactRecord`、`InMemoryArtifactStore`、`FileSystemArtifactStore` | 保存 bytes，并生成可重复 SHA-256 digest；filesystem backend 会落盘 bytes 和 v2 metadata，记录 size、content type、retention policy 和 retain-until timestamp，并在读取时重新校验 key、size 和 digest。 |
| SQLite | `sqlite.rs` | 仍是 durable backend 边界占位，V0.4 不引入 SQLite 依赖。 |

## 模块边界

`eva-storage` 只保存和返回数据，不解释 Agent 业务语义，不做 Topic 路由，不执行 Lua，也不决定 policy。调用方必须在写入前完成授权和 schema 判断。

`EventLog` 的 in-memory 实现只保证单进程测试闭环。`FileSystemEventLog` 提供跨进程重新打开后的查询和 replay 基线，但不负责 runtime crash recovery、调度重试或任务状态修复。未来 SQLite/WAL backend 接入时，应保持当前 trait 语义稳定：事件被 runtime 接受后先 append，处理完成后 ack 或 fail。

## 公开入口

```rust
use eva_storage::{
    EventLog, FileSystemEventLog, FileSystemTaskStateStore, InMemoryEventLog, InMemoryStateStore,
    StateStore,
};
```

主要 re-export 位于 `src/lib.rs`，下游 crate 不需要直接引用子模块路径。需要 durable event log 时使用 `FileSystemEventLog::open(backend.layout())`；需要兼容本地 task state 时使用 `FileSystemTaskStateStore::new(project_root)`；需要 durable backend task state 时使用 `FileSystemTaskStateStore::from_durable_layout(backend.layout())`；需要 durable artifact evidence 时使用 `FileSystemArtifactStore::new(path)`。

## 验证

当前模块验证命令：

```powershell
cargo test -p eva-storage
```

已覆盖：事件 append/watermark、ack consumer、fail structured error、replay cursor、filesystem EventLog 跨 reopen、StateStore 版本冲突、TaskStateStore 跨 store 读写和 snapshot 列表扫描、ArtifactStore digest round trip、filesystem artifact missing/tamper checks、legacy metadata 兼容和 corrupt metadata 稳定错误。

## V1.6.1 Durable Backend Baseline

`FileSystemDurableBackend` defines the first durable backend layout contract:

- `backend.manifest` records `schema_version=1` and layout version `eva.durable.v1`.
- `events/`, `state/`, `tasks/`, `audit/`, and `artifacts/` are created and verified as stable directories.
- `migration.lock` is acquired with `create_new` for read-write opens and released on drop.
- read-only open verifies an existing backend without creating files or taking a lock.
- `InMemoryDurableBackend` remains available as the test backend.

## V1.6.2 Durable Event Log

`FileSystemEventLog` writes versioned key/value records under `events/log`.
It persists event id, topic, target, payload, metadata, delivery status,
consumer, and structured error fields. Reopening the same durable backend can
replay records by sequence and keeps the next sequence watermark monotonic.

## V1.6.3 Durable Task Store Adapter

`FileSystemTaskStateStore` now has two entry points:

- `new(project_root)` keeps the compatible `.eva/tasks` diagnostic path.
- `from_durable_layout(layout)` uses the V1.6 durable backend `tasks/`
  directory for restart-readable task snapshots.

The CLI exposes this through `--durable-backend <path>` on `run --example basic`
and `task status/logs/cancel`.

`FileSystemAuditSink::open(layout)` writes audit records under the same backend
`audit/` directory. `query_by_trace_id` can retrieve records by span, request,
event, correlation, or causation id.

`FileSystemArtifactStore` writes versioned metadata next to artifact bytes. V2
metadata includes the key, digest, size, content type, retention policy, and
optional retain-until timestamp. Reads continue to accept legacy metadata
without a version field, then normalize missing content type and retention
fields to defaults while still returning stable conflicts for corrupt metadata.

## 后续计划

| 版本 | 计划 |
| --- | --- |
| V0.5 | 为 dead-letter 和任务日志增加查询索引。 |
| V1.6.3 | 已将 task snapshot、audit record 和 artifact metadata hardening 接入 durable backend 相关布局。 |
| V1.6.2 | 已新增 filesystem durable event log，供 `eva-eventbus::DurableEventBus` 使用。 |
| V1.2 | 接入 memory/knowledge 的持久化状态模型。 |
| V1.4 | 将 migration、snapshot、release artifact 命令接入 filesystem durable backend。 |
