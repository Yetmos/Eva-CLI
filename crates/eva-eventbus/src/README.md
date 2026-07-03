# eva-eventbus/src

更新时间：2026-07-03

本目录承载 V0.4 in-memory EventBus 和死信记录。

| 文件 | V0.4 状态 | 说明 |
| --- | --- | --- |
| `bus.rs` | 已实现 | `EventBus` trait 和 `EventReceipt`。 |
| `in_memory.rs` | 已实现 | `InMemoryEventBus`，内部使用 `eva-storage::InMemoryEventLog`。 |
| `dead_letter.rs` | 已实现 | `DeadLetterQueue` 和 `DeadLetterRecord`，保存无法投递事件及结构化原因。 |
| `recoverable.rs` | 边界保留 | future durable replay/recovery integration。 |
| `lib.rs` | 已实现 | re-export `EventBus`、`EventReceipt`、`InMemoryEventBus`、dead-letter 类型。 |

验证：`cargo test -p eva-eventbus`。
