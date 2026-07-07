# eva-storage/src

更新时间：2026-07-07

本目录承载存储 trait、in-memory 实现和 V1.6 durable filesystem backend。

| 文件 | 状态 | 说明 |
| --- | --- | --- |
| `audit_store.rs` | V1.6.3 已实现 | `FileSystemAuditSink`、`AuditRecord`。把 `AuditSink` 记录保存到 durable backend 的 `audit/` 目录，并支持按 trace id 查询。 |
| `event_log.rs` | V1.6.2 已更新 | `EventLog`、`EventLogRecord`、`EventLogStatus`、`InMemoryEventLog`、`FileSystemEventLog`。支持 append、ack、fail、watermark、replay 和跨 reopen 查询。 |
| `state_store.rs` | 已实现 | `StateStore`、`StateRecord`、`StateVersion`、`InMemoryStateStore`。支持 get、put、CAS。 |
| `task_state.rs` | V1.6.4 in progress | `TaskStateStore`、`TaskStateSnapshot`、`FileSystemTaskStateStore`。默认保存 `.eva/tasks` task snapshot，也可通过 `DurableBackendLayout` 使用 durable backend 的 `tasks/` 目录，支持跨进程读取 latest、指定 task 或 snapshot 列表。 |
| `artifact_store.rs` | V1.6.3 已实现 | `ArtifactStore`、`ArtifactRecord`、`InMemoryArtifactStore`、`FileSystemArtifactStore`。保存 bytes 并生成 SHA-256 digest；filesystem backend 写入 v2 metadata，记录 size、content type、retention policy 和 retain-until timestamp，并在读取时校验 key、size、digest。 |
| `sqlite.rs` | 边界保留 | 未来 SQLite/local durable backend。V0.4 不引入 SQLite 依赖。 |
| `lib.rs` | 已实现 | re-export 公开类型，供 eventbus/runtime 直接使用。 |

验证：`cargo test -p eva-storage`。

## V1.6.1 Durable Backend Baseline

`durable_backend.rs` owns the schema-versioned backend contract for local
durable storage. It exports `DurableBackend`, `DurableBackendOptions`,
`FileSystemDurableBackend`, and `InMemoryDurableBackend`. The filesystem
backend creates and verifies `events/`, `state/`, `tasks/`, `audit/`, and
`artifacts/`, uses `backend.manifest` for schema compatibility, and uses
`migration.lock` to prevent concurrent read-write migrations.

## V1.6.2 Durable Event Log

`FileSystemEventLog` stores records under `events/log` inside
`FileSystemDurableBackend`. Each file is a versioned key/value record that
keeps the original event, delivery status, consumer, error, and sequence. This
is the storage layer used by `eva-eventbus::DurableEventBus`; runtime crash
recovery and scheduler retry policy remain separate layers.

## V1.6.3 Durable Task Store Adapter

`FileSystemTaskStateStore::new(project_root)` keeps the legacy `.eva/tasks`
compatibility path. `FileSystemTaskStateStore::from_durable_layout(layout)` uses
the schema-versioned durable backend `tasks/` directory. The file format remains
the same line-oriented task snapshot format so `eva task status/logs/cancel`
can read either location without changing JSON output.

`FileSystemAuditSink::open(layout)` writes line-oriented audit records under the
same durable backend `audit/` directory. Each record stores action, outcome,
trace entries, message, fields, and a millisecond timestamp; `query_by_trace_id`
can retrieve records by span, request, event, correlation, or causation id.

`FileSystemArtifactStore` writes versioned metadata next to artifact bytes. V2
metadata stores key, digest, size, content type, retention policy, and optional
retain-until timestamp; legacy metadata without a version field is still read
with default content type and retention policy, while corrupt metadata returns a
stable conflict error.
