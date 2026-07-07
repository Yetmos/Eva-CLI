# eva-storage / 持久化存储边界

更新时间：2026-07-03

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-storage` 负责 Eva-CLI 的状态、事件日志、task snapshot 和 artifact 存储契约。事件和通用状态仍提供标准库 in-memory 版本；task snapshot 与 artifact 已提供 local filesystem backend，用于跨进程 CLI 查询、备份、发布和后续 apply evidence 的持久化边界。

## V0.4 当前实现

| 能力 | 类型/文件 | 当前行为 |
| --- | --- | --- |
| EventLog | `EventLog`、`InMemoryEventLog` | 支持 append、ack、fail、watermark、按 sequence replay。重复 event id 返回 `Conflict`。 |
| EventLogRecord | `EventLogRecord`、`EventLogStatus` | 记录 sequence、原始 `Event`、消费 Agent、失败原因。 |
| StateStore | `StateStore`、`InMemoryStateStore` | 支持 get、put、compare-and-set；版本从 1 单调递增。 |
| StateRecord | `StateRecord`、`StateVersion` | 保存 key、value 和 CAS version。 |
| TaskStateStore | `TaskStateStore`、`FileSystemTaskStateStore` | 保存 `.eva/tasks` task snapshot，支持按 task id 或 latest 跨进程读取。 |
| ArtifactStore | `ArtifactStore`、`InMemoryArtifactStore`、`FileSystemArtifactStore` | 保存 bytes，并生成可重复 SHA-256 digest；filesystem backend 会落盘 bytes 和 metadata，并在读取时重新校验 digest。 |
| SQLite | `sqlite.rs` | 仍是 durable backend 边界占位，V0.4 不引入 SQLite 依赖。 |

## 模块边界

`eva-storage` 只保存和返回数据，不解释 Agent 业务语义，不做 Topic 路由，不执行 Lua，也不决定 policy。调用方必须在写入前完成授权和 schema 判断。

`EventLog` 的 V0.4 in-memory 实现只保证单进程测试闭环，不提供进程崩溃恢复。未来 SQLite/WAL backend 接入时，应保持当前 trait 语义稳定：事件被 runtime 接受后先 append，处理完成后 ack 或 fail。

## 公开入口

```rust
use eva_storage::{
    EventLog, FileSystemTaskStateStore, InMemoryEventLog, InMemoryStateStore, StateStore,
};
```

主要 re-export 位于 `src/lib.rs`，下游 crate 不需要直接引用子模块路径。需要 durable task state 时使用 `FileSystemTaskStateStore::new(project_root)`；需要 durable artifact evidence 时使用 `FileSystemArtifactStore::new(path)`。

## 验证

当前模块验证命令：

```powershell
cargo test -p eva-storage
```

V0.4 已覆盖：事件 append/watermark、ack consumer、fail structured error、replay cursor、StateStore 版本冲突、TaskStateStore 跨 store 读写、ArtifactStore digest round trip、filesystem artifact missing/tamper checks。

## V1.6.1 Durable Backend Baseline

`FileSystemDurableBackend` defines the first durable backend layout contract:

- `backend.manifest` records `schema_version=1` and layout version `eva.durable.v1`.
- `events/`, `state/`, `tasks/`, `audit/`, and `artifacts/` are created and verified as stable directories.
- `migration.lock` is acquired with `create_new` for read-write opens and released on drop.
- read-only open verifies an existing backend without creating files or taking a lock.
- `InMemoryDurableBackend` remains available as the test backend.

## 后续计划

| 版本 | 计划 |
| --- | --- |
| V0.5 | 为 dead-letter 和任务日志增加查询索引。 |
| V1.2 | 接入 memory/knowledge 的持久化状态模型。 |
| V1.4 | 将 migration、snapshot、release artifact 命令接入 filesystem durable backend。 |
