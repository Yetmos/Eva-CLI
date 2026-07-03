# eva-storage/src

更新时间：2026-07-03

本目录承载 V0.4 存储 trait 和 in-memory 实现。

| 文件 | V0.4 状态 | 说明 |
| --- | --- | --- |
| `event_log.rs` | 已实现 | `EventLog`、`EventLogRecord`、`EventLogStatus`、`InMemoryEventLog`。支持 append、ack、fail、watermark、replay。 |
| `state_store.rs` | 已实现 | `StateStore`、`StateRecord`、`StateVersion`、`InMemoryStateStore`。支持 get、put、CAS。 |
| `artifact_store.rs` | 已实现 | `ArtifactStore`、`ArtifactRecord`、`InMemoryArtifactStore`。保存 bytes 并生成轻量 digest。 |
| `sqlite.rs` | 边界保留 | 未来 SQLite/local durable backend。V0.4 不引入 SQLite 依赖。 |
| `lib.rs` | 已实现 | re-export V0.4 公开类型，供 eventbus/runtime 直接使用。 |

验证：`cargo test -p eva-storage`。
