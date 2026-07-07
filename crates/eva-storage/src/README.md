# eva-storage/src

更新时间：2026-07-03

本目录承载 V0.4 存储 trait 和 in-memory 实现。

| 文件 | V0.4 状态 | 说明 |
| --- | --- | --- |
| `event_log.rs` | 已实现 | `EventLog`、`EventLogRecord`、`EventLogStatus`、`InMemoryEventLog`。支持 append、ack、fail、watermark、replay。 |
| `state_store.rs` | 已实现 | `StateStore`、`StateRecord`、`StateVersion`、`InMemoryStateStore`。支持 get、put、CAS。 |
| `task_state.rs` | 已实现 | `TaskStateStore`、`TaskStateSnapshot`、`FileSystemTaskStateStore`。保存 `.eva/tasks` task snapshot，支持跨进程读取 latest 或指定 task。 |
| `artifact_store.rs` | 已实现 | `ArtifactStore`、`ArtifactRecord`、`InMemoryArtifactStore`、`FileSystemArtifactStore`。保存 bytes 并生成 SHA-256 digest；filesystem backend 会校验 metadata 与 bytes 是否一致。 |
| `sqlite.rs` | 边界保留 | 未来 SQLite/local durable backend。V0.4 不引入 SQLite 依赖。 |
| `lib.rs` | 已实现 | re-export V0.4 公开类型，供 eventbus/runtime 直接使用。 |

验证：`cargo test -p eva-storage`。

## V1.6.1 Durable Backend Baseline

`durable_backend.rs` owns the schema-versioned backend contract for local
durable storage. It exports `DurableBackend`, `DurableBackendOptions`,
`FileSystemDurableBackend`, and `InMemoryDurableBackend`. The filesystem
backend creates and verifies `events/`, `state/`, `tasks/`, `audit/`, and
`artifacts/`, uses `backend.manifest` for schema compatibility, and uses
`migration.lock` to prevent concurrent read-write migrations.
