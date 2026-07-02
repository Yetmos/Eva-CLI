# eva-scheduler/src / 调度源码

![V0.3/V0.4 runtime module flow](../../assets/eva-runtime-module-flow.svg)

本目录承载 Topic route、Agent subscription 和 mailbox 投递。当前为骨架，V0.4 先实现 exact/wildcard 路由和 bounded mailbox。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 骨架 | V0.4 |
| `registry.rs` | Agent mailbox registry | 骨架 | V0.4 |
| `routing.rs` | route rule 和 delivery 语义 | 骨架 | V0.4 |
| `subscription.rs` | Agent subscription table | 骨架 | V0.4 |
| `matcher.rs` | TopicPattern 匹配和优先级 | 骨架 | V0.4 |
| `mailbox.rs` | bounded mailbox delivery | 骨架 | V0.4 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义 route、subscription、delivery result。 | 配置 routes 可转内部模型。 |
| 2 | 实现 matcher 和优先级。 | exact、`*`、`**` 测试通过。 |
| 3 | 实现 mailbox registry 和 bounded send。 | Agent 可接收投递。 |
| 4 | 接 EventBus consumer 和 AgentRuntime。 | V0.4 事件到 Agent。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Registry | Agent mailbox 元数据 | 未实现 | 定义 handle 和 capacity。 |
| Routing | 路由规则 | 未实现 | 接 `eva-config` routes。 |
| Subscription | Agent 订阅 | 未实现 | 从 manifest 构造订阅项。 |
| Matcher | Topic 匹配 | 未实现 | 复用 `TopicPattern`。 |
| Mailbox | bounded delivery | 未实现 | 实现 overflow 错误。 |
