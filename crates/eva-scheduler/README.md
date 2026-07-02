# eva-scheduler / 调度器

更新时间：2026-07-02

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-scheduler` 负责 Topic 路由、订阅表和 Agent mailbox 投递。它把 `eva-core::Topic`、`TopicPattern`、配置 routes 和 Agent subscription 转成可执行的投递决策，但不执行 Lua，不调用 Adapter，也不保存业务状态。

## 当前模块功能说明

| 功能域 | 当前状态 | 目标行为 |
| --- | --- | --- |
| Registry | 骨架 | 维护 Agent mailbox 元数据、状态和可投递性。 |
| Routing | 骨架 | 表示 route rule、target、delivery 语义和优先级。 |
| Subscription | 骨架 | 保存 Agent 订阅的 TopicPattern 和过滤条件。 |
| Matcher | 骨架 | 复用 `eva-core::TopicPattern` 做 exact、`*`、尾部 `**` 匹配。 |
| Mailbox | 骨架 | 将匹配事件投递到 Agent bounded mailbox。 |
| Fairness/backpressure | 未实现 | 避免单个 Agent 或 Topic 占满队列。 |

## 模块边界

`eva-scheduler` 做：

- 把 routes、subscriptions、direct target 转成投递列表。
- 维护 mailbox 元数据和投递结果。
- 输出可审计的路由决策和失败原因。

`eva-scheduler` 不做：

- 不发布或持久化事件，这属于 `eva-eventbus` 和 `eva-storage`。
- 不执行 Agent handler 或 Lua。
- 不解释 payload。
- 不越过 policy 给未授权 Agent 投递敏感事件。

## 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V0.4 | 定义 route rule、subscription entry、delivery target 和 route result。 | `eva-core`、`eva-config` routes | 数据结构可序列化为 CLI inspect 输出。 |
| 2 | V0.4 | 实现 matcher：exact、`*` 单段、尾部 `**`，明确优先级。 | `eva-core::TopicPattern` | 路由测试覆盖所有匹配类型。 |
| 3 | V0.4 | 实现 Agent registry 和 mailbox metadata。 | `eva-core::AgentId` | 注册、注销、禁用 Agent 可测。 |
| 4 | V0.4 | 实现 bounded mailbox 投递和投递失败原因。 | 标准库 | 队列满、Agent 不可用时返回结构化错误。 |
| 5 | V0.4 | 接 EventBus consumer，将事件投递给 AgentRuntime。 | `eva-eventbus`、`eva-agent` | 最小 runtime 闭环中事件能到 Agent。 |
| 6 | V0.5 | 增加公平性、重试策略、超时取消传播。 | AgentRuntime | 长任务不会阻塞全部路由。 |

## 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 模块导出 | 骨架 | re-export registry、routing、subscription、matcher、mailbox。 |
| `src/registry.rs` | Agent mailbox 注册表 | `RESPONSIBILITY` 占位 | 定义 mailbox handle、agent status、capacity。 |
| `src/routing.rs` | 路由规则模型 | `RESPONSIBILITY` 占位 | 定义 direct/broadcast/round-robin 等 delivery 初版。 |
| `src/subscription.rs` | Agent 订阅表 | `RESPONSIBILITY` 占位 | 从 manifest subscription 构建订阅项。 |
| `src/matcher.rs` | Topic 匹配 | `RESPONSIBILITY` 占位 | 复用 `TopicPattern::matches` 并定义优先级。 |
| `src/mailbox.rs` | mailbox 投递 | `RESPONSIBILITY` 占位 | 实现 bounded send、overflow、closed 状态。 |
| `src/README.md` | 源码目录说明 | 简略 | 补充文件职责和进度。 |
| 单元测试 | 路由和投递 | 未开始 | 覆盖 target、exact、wildcard、queue full。 |

## 验证计划

| 阶段 | 命令 | 目标 |
| --- | --- | --- |
| V0.4 | `cargo test -p eva-scheduler` | 路由匹配和 mailbox 投递可测。 |
| V0.4 | `cargo test -p eva-runtime` | runtime 能装配 scheduler 与 AgentRuntime。 |
| V0.4 | `cargo run -- run --example basic` | 事件按 Topic 到达 Agent。 |

## English

`eva-scheduler` owns Topic routing, subscription tables, and Agent mailbox delivery. It does not execute Lua, call Adapters, or persist event logs.
