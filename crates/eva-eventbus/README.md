# eva-eventbus / 事件总线

更新时间：2026-07-07

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-eventbus` 负责事件发布、消费确认、失败记录和死信诊断。V1.6.2 在原有 `InMemoryEventBus` 之外新增 `DurableEventBus`，把 publish/ack/fail 写入 durable event log，并把 dead-letter redrive 队列落盘到 V1.6 durable backend。

## 当前实现

| 能力 | 类型/文件 | 当前行为 |
| --- | --- | --- |
| 发布契约 | `EventBus::publish` | 接收 `eva_core::Event`，append 到 EventLog，返回 `EventReceipt`。 |
| 消费确认 | `EventBus::ack` | 将事件日志记录标记为 acked，并记录 consumer Agent。 |
| 消费失败 | `EventBus::fail` | 将事件日志记录标记为 failed，并保留 `EvaError`。 |
| 死信队列 | `DeadLetterQueue` | runtime 可将无法投递或处理失败的事件和结构化原因写入内存死信队列。 |
| 死信 replay | `DeadLetterQueue::replay_all_for_publish`、`InMemoryEventBus::replay_dead_letters` | 生成 `:replay-N` 子事件并重新 publish，返回 replay receipts。 |
| in-memory bus | `InMemoryEventBus` | 内含 `InMemoryEventLog`、receipt 列表和 dead-letter queue。 |
| durable bus | `DurableEventBus`、`FileSystemDeadLetterStore` | 使用 `FileSystemEventLog` 保存 publish/ack/fail；dead-letter 记录保存为可重启查询的文件，并在批量或单事件 redrive 时生成新的 delivery attempt。 |

## V1.6.2 Redrive 语义

- replay 不覆盖原始 event id；会生成 `原始ID:replay-N` 子事件，避免 EventLog 冲突。
- replay 会增加 `DeadLetterRecord.replay_count`，供 runtime task report 输出。
- `DeadLetterRecord.redrive` 包含 `retry_delay_ms` 与 `next_attempt_after_ms`，当前默认策略仍是立即 redrive，字段已进入 durable 序列化以便后续接入 backoff policy。
- durable redrive 只负责保存和重新发布事件，不决定 scheduler 路由、consumer 选择或任务恢复状态；V1.6.4 runtime recovery 使用单事件 redrive 入口按 task/event 证据筛选候选。

## 模块边界

`eva-eventbus` 不做 Topic 匹配，不维护 Agent 订阅表，不执行 Lua，不调用 capability。Topic 到 Agent 的投递由 `eva-scheduler` 完成。V1.6.2 的 durable redrive 只提供事件生命周期证据和持久化死信队列；crash recovery coordinator、任务状态恢复和 backoff 调度属于后续 runtime 切片。

## 公开入口

```rust
use eva_eventbus::{DeadLetterRecord, DurableEventBus, EventBus, EventReceipt, InMemoryEventBus};
```

## 验证

```powershell
cargo test -p eva-eventbus
```

已覆盖：publish 写入日志并返回 receipt、ack 更新日志、dead-letter 记录失败原因、单事件 replay、dead-letter replay 到 EventLog、durable publish/ack/fail 跨 reopen、durable dead-letter redrive、runtime 单事件 redrive checkpoint、backoff 字段兼容读取。

## 后续计划

| 版本 | 计划 |
| --- | --- |
| V1.0 | 已将 dead-letter replay 报告纳入 quickstart 故障诊断和 release notes。 |
| V1.6.2 | 已接入 durable EventBus、可查询 dead-letter store 和默认 redrive 字段。 |
| V1.x | 接入 observability metrics、真实 backoff 调度和 runtime crash recovery。 |
