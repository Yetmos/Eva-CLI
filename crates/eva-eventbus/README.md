# eva-eventbus / 事件总线

更新时间：2026-07-02

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-eventbus` 负责事件发布、订阅分发入口、可恢复日志集成和死信路径。它处理事件的传递可靠性和生命周期，不拥有 Agent Topic 订阅匹配规则，不执行 Lua，不保存业务状态。

## 当前模块功能说明

| 功能域 | 当前状态 | 目标行为 |
| --- | --- | --- |
| Bus contract | 骨架 | 定义 publish、subscribe、ack、fail、close 等事件总线接口。 |
| In-memory bus | 骨架 | V0.4 提供测试和本地闭环可用的内存实现。 |
| Recoverable log | 骨架 | 与 `eva-storage::EventLog` 集成，支持 replay、watermark 和恢复消费。 |
| Dead letter | 骨架 | 失败超过策略后写入 dead-letter，保留原因和 trace。 |
| Backpressure | 未实现 | mailbox 和 bus 应有容量限制和拒绝策略。 |
| Observability | 未实现 | 每次 publish、ack、fail、dead-letter 输出 trace/audit/metrics。 |

## 模块边界

`eva-eventbus` 做：

- 接收已构造的 `eva-core::Event`。
- 管理事件递送、确认、失败、重放和死信。
- 暴露可测试的总线 trait 和 in-memory 实现。

`eva-eventbus` 不做：

- 不计算 Topic 路由和 Agent 订阅匹配，这属于 `eva-scheduler`。
- 不执行 Agent、Lua、Adapter 或 capability。
- 不把事件日志当业务状态库。
- 不决定是否允许 publish，调用方必须先完成 policy 检查。

## 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V0.4 | 定义 `EventBus` trait、publisher、consumer、ack token 和错误类型。 | `eva-core` | trait 不绑定具体 runtime。 |
| 2 | V0.4 | 实现 in-memory publish/subscribe，支持 topic/target 由调用方传入。 | 标准库 | 单线程和多订阅者测试通过。 |
| 3 | V0.4 | 接入 `EventLog`，publish 后 append，ack/fail 更新状态。 | `eva-storage` | replay 可以恢复未 ack 事件。 |
| 4 | V0.4 | 实现 dead-letter 记录，保留失败原因、attempt、trace。 | `eva-storage`、`eva-observability` | 超过重试上限后可查询 dead-letter。 |
| 5 | V0.5 | 增加 backpressure、retry delay、cancel-aware close。 | `eva-agent` | 长任务取消时不会丢失确认状态。 |
| 6 | V1.x | 对接 runtime metrics 和审计后端。 | `eva-observability` | publish/ack/fail 有统一指标。 |

## 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 模块导出 | 骨架 | re-export bus、in-memory、recoverable、dead-letter。 |
| `src/bus.rs` | EventBus trait | `RESPONSIBILITY` 占位 | 定义 publish、claim、ack、fail、subscribe API。 |
| `src/in_memory.rs` | 内存总线实现 | `RESPONSIBILITY` 占位 | 实现 bounded queue 和订阅者通知。 |
| `src/recoverable.rs` | 日志恢复集成 | `RESPONSIBILITY` 占位 | 定义 replay cursor 和未确认事件恢复。 |
| `src/dead_letter.rs` | 死信记录 | `RESPONSIBILITY` 占位 | 定义 dead-letter record 和查询接口。 |
| `src/README.md` | 源码目录说明 | 简略 | 补充文件职责和进度。 |
| 单元测试 | 事件生命周期 | 未开始 | 覆盖 publish、ack、fail、replay、dead-letter。 |

## 验证计划

| 阶段 | 命令 | 目标 |
| --- | --- | --- |
| V0.4 | `cargo test -p eva-eventbus` | 内存总线和恢复语义可测。 |
| V0.4 | `cargo test -p eva-runtime` | runtime 可以装配 EventBus。 |
| V0.5 | CLI task logs 测试 | dead-letter 和失败日志可查询。 |

## English

`eva-eventbus` owns event delivery, recovery-log integration, and dead-letter paths. Topic matching belongs to `eva-scheduler`; execution belongs to Agent, Lua, Adapter, or Capability modules.
