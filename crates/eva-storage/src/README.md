# eva-storage/src / 存储源码

![V0.3/V0.4 runtime module flow](../../assets/eva-runtime-module-flow.svg)

本目录承载状态存储、事件日志、artifact 存储和本地实现边界。当前为骨架，V0.4 先提供 trait 和 in-memory 实现。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 骨架 | V0.4 |
| `state_store.rs` | StateStore trait、key、version、CAS | 骨架 | V0.4 |
| `event_log.rs` | EventLog append、ack、fail、replay | 骨架 | V0.4 |
| `artifact_store.rs` | ArtifactStore、digest、metadata | 骨架 | V0.4/V1.4 |
| `sqlite.rs` | SQLite/local implementation boundary | 骨架 | V0.4+ |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义 StateStore 和 EventLog trait。 | EventBus 可依赖接口。 |
| 2 | 实现 in-memory store。 | V0.4 测试不依赖 SQLite。 |
| 3 | 定义 ArtifactStore 和 digest 校验。 | Backup 可复用 artifact 边界。 |
| 4 | 再补 SQLite/local durable 实现。 | 本地运行具备恢复能力。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| StateStore | 版本化状态读写 | 未实现 | 设计 key 和 CAS。 |
| EventLog | 可恢复事件日志 | 未实现 | 设计 cursor 和 ack。 |
| ArtifactStore | artifact 保存和校验 | 未实现 | 设计 digest envelope。 |
| SQLite | 本地持久化 | 未实现 | 等 trait 稳定后实现。 |
