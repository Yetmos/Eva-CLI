# eva-eventbus/src

更新时间：2026-07-07

本目录承载 in-memory EventBus、durable EventBus、死信记录和 replay/redrive 入口。

| 文件 | 状态 | 说明 |
| --- | --- | --- |
| `bus.rs` | 已实现 | `EventBus` trait 和 `EventReceipt`。 |
| `in_memory.rs` | 已更新 | `InMemoryEventBus`，内部使用 `eva-storage::InMemoryEventLog`，并支持 `replay_dead_letters`。 |
| `durable.rs` | V1.6.4 已更新 | `DurableEventBus` 和 `FileSystemDeadLetterStore`，保存 publish/ack/fail 与 durable dead-letter redrive；支持恢复检查点需要的单事件 redrive 和 redrive policy 更新。 |
| `dead_letter.rs` | 已更新 | `DeadLetterQueue`、`DeadLetterRecord`、`RedrivePolicy`、`replay_count`、单事件和批量 replay。 |
| `recoverable.rs` | 边界保留 | future runtime crash recovery integration。 |
| `lib.rs` | 已实现 | re-export `EventBus`、`EventReceipt`、`InMemoryEventBus`、`DurableEventBus`、dead-letter 类型。 |

## V1.6.2 注意事项

`replay_dead_letters` 会创建带 `:replay-N` 后缀的新事件 ID，并重新写入同一个 EventLog。`DurableEventBus` 使用 `events/log` 保存事件日志，使用 `events/dead_letters` 保存可重启查询的死信记录。`RedrivePolicy` 当前默认值为 0ms，字段已序列化但不在本层执行延迟调度。

## V1.12.4 更新

`DurableEventBus` now exposes due dead-letter selection and replay-state queries:
`due_dead_letters`, `event_log_status`, `event_log_record`, and
`latest_replay_record`. Runtime scheduler ticks and recovery checkpoints use
these queries to avoid duplicate redrive after a replay event has already been
published, acked, or failed.

验证：`cargo test -p eva-eventbus`。
