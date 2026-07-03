# eva-eventbus / 事件总线

更新时间：2026-07-03

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-eventbus` 负责事件发布、消费确认、失败记录和死信诊断。V0.5 在 V0.4 `InMemoryEventBus` 上增加 dead-letter replay 入口，用于 basic runtime 的失败路径报告。

## 当前实现

| 能力 | 类型/文件 | 当前行为 |
| --- | --- | --- |
| 发布契约 | `EventBus::publish` | 接收 `eva_core::Event`，append 到 EventLog，返回 `EventReceipt`。 |
| 消费确认 | `EventBus::ack` | 将事件日志记录标记为 acked，并记录 consumer Agent。 |
| 消费失败 | `EventBus::fail` | 将事件日志记录标记为 failed，并保留 `EvaError`。 |
| 死信队列 | `DeadLetterQueue` | runtime 可将无法投递或处理失败的事件和结构化原因写入内存死信队列。 |
| 死信 replay | `DeadLetterQueue::replay_all_for_publish`、`InMemoryEventBus::replay_dead_letters` | 生成 `:replay-N` 子事件并重新 publish，返回 replay receipts。 |
| in-memory bus | `InMemoryEventBus` | 内含 `InMemoryEventLog`、receipt 列表和 dead-letter queue。 |

## V0.5 Replay 语义

- replay 不覆盖原始 event id；会生成 `原始ID:replay-N` 子事件，避免 EventLog 冲突。
- replay 会增加 `DeadLetterRecord.replay_count`，供 runtime task report 输出。
- 当前 replay 仍是 in-memory 诊断能力，不是 durable redrive queue。

## 模块边界

`eva-eventbus` 不做 Topic 匹配，不维护 Agent 订阅表，不执行 Lua，不调用 capability。Topic 到 Agent 的投递由 `eva-scheduler` 完成。V0.5 的 dead-letter/replay 只提供事件生命周期证据，长期持久化和 backoff policy 属于后续 storage/runtime 切片。

## 公开入口

```rust
use eva_eventbus::{EventBus, EventReceipt, InMemoryEventBus, DeadLetterRecord};
```

## 验证

```powershell
cargo test -p eva-eventbus
```

已覆盖：publish 写入日志并返回 receipt、ack 更新日志、dead-letter 记录失败原因、单事件 replay、dead-letter replay 到 EventLog。

## 后续计划

| 版本 | 计划 |
| --- | --- |
| V1.0 | 已将 dead-letter replay 报告纳入 quickstart 故障诊断和 release notes。 |
| V1.x | 接入 observability metrics、durable dead-letter 查询、retry delay/backoff。 |
