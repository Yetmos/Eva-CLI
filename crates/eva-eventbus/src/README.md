# eva-eventbus/src

更新时间：2026-07-03

本目录承载 in-memory EventBus、死信记录和 V0.5 replay 诊断入口。

| 文件 | V0.5 状态 | 说明 |
| --- | --- | --- |
| `bus.rs` | 已实现 | `EventBus` trait 和 `EventReceipt`。 |
| `in_memory.rs` | 已更新 | `InMemoryEventBus`，内部使用 `eva-storage::InMemoryEventLog`，并支持 `replay_dead_letters`。 |
| `dead_letter.rs` | 已更新 | `DeadLetterQueue`、`DeadLetterRecord`、`replay_count`、单事件和批量 replay。 |
| `recoverable.rs` | 边界保留 | future durable replay/recovery integration。 |
| `lib.rs` | 已实现 | re-export `EventBus`、`EventReceipt`、`InMemoryEventBus`、dead-letter 类型。 |

## V0.5 注意事项

`replay_dead_letters` 会创建带 `:replay-N` 后缀的新事件 ID，并重新写入同一个 in-memory log。该能力用于 task report 证明“失败事件可回放”，不是稳定持久化 redrive API。

验证：`cargo test -p eva-eventbus`。
