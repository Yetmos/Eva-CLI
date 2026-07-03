# eva-eventbus / 事件总线

更新时间：2026-07-03

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-eventbus` 负责事件发布、消费确认和死信记录。V0.4 已实现 `InMemoryEventBus`，用于最小运行闭环：CLI 构造事件后发布到 EventBus，EventBus 写入 `eva-storage::EventLog`，后续由 runtime 调用 scheduler 投递给 Agent。

## V0.4 当前实现

| 能力 | 类型/文件 | 当前行为 |
| --- | --- | --- |
| 发布契约 | `EventBus::publish` | 接收 `eva_core::Event`，append 到 EventLog，返回 `EventReceipt`。 |
| 消费确认 | `EventBus::ack` | 将事件日志记录标记为 acked，并记录 consumer Agent。 |
| 消费失败 | `EventBus::fail` | 将事件日志记录标记为 failed，并保留 `EvaError`。 |
| 死信队列 | `DeadLetterQueue` | runtime 可将无法投递的事件和结构化原因写入内存死信队列。 |
| in-memory bus | `InMemoryEventBus` | 内含 `InMemoryEventLog`、receipt 列表和 dead-letter queue。 |

## 模块边界

`eva-eventbus` 不做 Topic 匹配，不维护 Agent 订阅表，不执行 Lua，不调用 capability。它只管理事件进入总线后的生命周期证据。Topic 到 Agent 的投递由 `eva-scheduler` 完成。

V0.4 的 dead-letter 是内存记录，主要覆盖失败路径和报告证据。它不是 durable dead-letter store。后续 durable dead-letter 查询应落在 `eva-storage` backend 上。

## 公开入口

```rust
use eva_eventbus::{EventBus, EventReceipt, InMemoryEventBus};
```

## 验证

当前模块验证命令：

```powershell
cargo test -p eva-eventbus
```

V0.4 已覆盖：publish 写入日志并返回 receipt、ack 更新日志、dead-letter 记录失败原因。

## 后续计划

| 版本 | 计划 |
| --- | --- |
| V0.5 | 增加 backpressure、retry delay、cancel-aware close。 |
| V1.x | 接入 observability metrics 和 durable dead-letter 查询。 |
