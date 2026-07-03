# eva-scheduler / Topic 调度器

更新时间：2026-07-03

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-scheduler` 负责把事件 Topic 和显式 target 转换为 Agent mailbox 投递计划。V0.4 已实现 route rule、TopicPattern 匹配、direct target 优先、fanout/compete 的最小语义，以及 bounded mailbox 投递。

## V0.4 当前实现

| 能力 | 类型/文件 | 当前行为 |
| --- | --- | --- |
| 路由规则 | `RoutingRule`、`DeliveryMode` | 表示 pattern、delivery mode 和目标 Agent 列表。 |
| Topic 匹配 | `matching_rules` | 复用 `eva-core::TopicPattern::matches`，保持 route 顺序。 |
| 订阅表 | `SubscriptionTable` | `route` 生成 `DeliveryPlan`，`deliver` 写入 mailbox。 |
| 显式 target | `EventTarget::Agent` | 直接返回目标 Agent，优先于 Topic route。 |
| fanout | `DeliveryMode::Fanout` | 对 route 中所有 Agent 生成投递计划。 |
| compete | `DeliveryMode::Compete` | V0.4 选取 route 的第一个 Agent，后续再扩展公平竞争。 |
| mailbox | `AgentMailbox`、`MailboxRegistry` | bounded FIFO；容量满返回 `Unavailable`。 |

## 模块边界

`eva-scheduler` 不发布事件，不持久化日志，不执行 Agent/Lua，也不解释 payload。它只决定“这个事件应该交给哪些 Agent”。runtime 负责从配置构造 `SubscriptionTable`，并将 mailbox 中的事件交给 `eva-agent`。

## 公开入口

```rust
use eva_scheduler::{DeliveryMode, MailboxRegistry, RoutingRule, SubscriptionTable};
```

## 验证

当前模块验证命令：

```powershell
cargo test -p eva-scheduler
```

V0.4 已覆盖：wildcard 匹配、bounded FIFO、fanout route、direct target override、deliver 写入 mailbox。

## 后续计划

| 版本 | 计划 |
| --- | --- |
| V0.5 | 增加公平竞争、retry/backoff、Agent disable/drain 语义。 |
| V1.x | 接入更完整的 policy gate 和跨 generation route 切换。 |
