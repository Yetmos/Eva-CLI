# eva-storage / 持久化存储

更新时间：2026-07-02

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-storage` 负责 Eva-CLI 的存储接口和本地实现边界，包括状态存储、可恢复事件日志、artifact 存储和 SQLite/local 实现。它保存恢复所需的数据，不承载 Agent 业务逻辑，不解释 Lua payload，不决定事件路由。

## 当前模块功能说明

| 功能域 | 当前状态 | 目标行为 |
| --- | --- | --- |
| StateStore | 骨架 | 保存 runtime、Agent、capability、hardware 等状态快照和版本。 |
| EventLog | 骨架 | 记录事件 append、ack、fail、watermark、replay 所需数据。 |
| ArtifactStore | 骨架 | 保存 backup、snapshot、migration package、provider artifact 的内容和 digest。 |
| SQLite/local 实现 | 骨架 | 提供本地 durable 实现，测试可先使用 in-memory 实现。 |
| 事务边界 | 未实现 | append event 与状态更新要有明确一致性策略。 |
| 数据迁移 | 未实现 | V1.4 与 `eva-backup` 协作输出可校验迁移包。 |

## 模块边界

`eva-storage` 做：

- 定义存储 trait、key、record、cursor、digest 等持久化契约。
- 为 runtime/eventbus/memory/backup 提供可测试的本地接口。
- 维护 event replay 和 dead-letter 查询需要的索引。

`eva-storage` 不做：

- 不执行 Agent 业务逻辑。
- 不解析 capability schema 或 provider 私有协议。
- 不决定 policy，调用方必须先完成授权。
- 不把 EventBus 当成业务状态库，业务状态应进入明确的 state/memory 表。

## 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V0.4 | 定义 `StateStore` trait、状态 key、版本号和 compare-and-set 语义。 | `eva-core` | 内存实现可测试并发写冲突。 |
| 2 | V0.4 | 定义 `EventLog` trait、append、ack、fail、cursor、watermark。 | `eva-core::Event` | 可恢复消费测试通过。 |
| 3 | V0.4 | 定义 `ArtifactStore` trait、digest、metadata、stream/bytes 边界。 | `eva-core::EvaError` | artifact 写入后可校验 digest。 |
| 4 | V0.4 | 实现 in-memory storage，用于 runtime 闭环和单元测试。 | 标准库 | EventBus 和 Agent 测试不依赖 SQLite。 |
| 5 | V0.5 | 增加 dead-letter 查询和任务日志读取所需索引。 | `eva-eventbus` | CLI 可查询失败事件和日志摘要。 |
| 6 | V1.2 | 增加 memory/knowledge 存储表或 trait 分层。 | `eva-memory` | Agent 私有记忆和全局知识可隔离。 |
| 7 | V1.4 | 增加 migration、snapshot、restore plan 支持。 | `eva-backup` | migration package 可导出和验证。 |

## 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 模块导出 | 骨架 | re-export trait、record、error helper。 |
| `src/state_store.rs` | 状态存储接口 | `RESPONSIBILITY` 占位 | 定义 key、value envelope、version、CAS。 |
| `src/event_log.rs` | 可恢复事件日志 | `RESPONSIBILITY` 占位 | 定义 append、claim、ack、fail、replay cursor。 |
| `src/artifact_store.rs` | artifact 存储接口 | `RESPONSIBILITY` 占位 | 定义 artifact id、digest、metadata、verify。 |
| `src/sqlite.rs` | SQLite/local 实现边界 | `RESPONSIBILITY` 占位 | 先保留接口，V0.4 以 in-memory 实现优先。 |
| `src/README.md` | 源码目录说明 | 简略 | 补充文件职责和开发状态。 |
| 单元测试 | in-memory store | 未开始 | 覆盖状态版本、事件 ack/fail、digest 校验。 |

## 验证计划

| 阶段 | 命令 | 目标 |
| --- | --- | --- |
| V0.4 | `cargo test -p eva-storage` | trait 模型和 in-memory 实现可测。 |
| V0.4 | `cargo test -p eva-eventbus` | EventBus 可使用 EventLog 接口。 |
| V1.4 | backup/migration 集成测试 | artifact 和 manifest digest 可验证。 |

## English

`eva-storage` owns persistence interfaces and local implementation boundaries for state, durable events, and artifacts. It does not own Agent business logic, routing, or policy decisions.
