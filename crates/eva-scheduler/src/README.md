# eva-scheduler/src

更新时间：2026-07-03

本目录承载 V0.4 Topic route 到 Agent mailbox 的最小调度闭环。

| 文件 | V0.4 状态 | 说明 |
| --- | --- | --- |
| `routing.rs` | 已实现 | `RoutingRule` 和 `DeliveryMode`。 |
| `matcher.rs` | 已实现 | `matching_rules`，复用 `TopicPattern::matches`。 |
| `subscription.rs` | 已实现 | `SubscriptionTable`、`DeliveryPlan`，支持 direct target、fanout、compete-first。 |
| `mailbox.rs` | 已实现 | `AgentMailbox` bounded FIFO。 |
| `registry.rs` | 已实现 | `MailboxRegistry` 注册、投递、drain_one。 |
| `lib.rs` | 已实现 | re-export V0.4 公开类型。 |

验证：`cargo test -p eva-scheduler`。
