# Topic 路由事实源与校验边界

> Language: 简体中文
> English default entry: [English](../../en/architecture/topic-routing-hybrid-sync.md)
> Translation status: current

更新日期：2026-07-13

## 文档定位

本文记录当前 runtime Topic route table 与 Agent manifest 之间已经实现的关系。

**从 `config/eva.yaml` 解析出的 route 文件（通常为
`config/routes/topics.yaml`）是 Scheduler Topic 投递的唯一运行时事实源。**
Agent `subscriptions` 是会被解析和展示的声明，但不会生成、合并或替换 route
table。

Eva-CLI 只读取这份配置。启动、校验、Agent reload 和 daemon control 都不会
写入 route。

## 配置归属

| 输入 | 已实现职责 | 是否拥有路由决定权 |
| --- | --- | --- |
| `config/eva.yaml` | 选择配置根，包括 `config.route_file` | 指向 route 事实源 |
| `config/routes/topics.yaml` | 定义有序的 `pattern`、`delivery` 和 `agents` 规则 | 是 |
| `config/agents/**/agent.yaml` 的 `subscriptions` | 声明与 Agent 关联的 Topic pattern，并暴露给 inspect/status | 否 |
| `config/agents/**/agent.yaml` 的 `permissions.emit` | 声明 Agent 允许 emit 的 Topic pattern | 否 |
| Agent `parent` 与 `children` | 声明管理关系 | 否 |

当前契约没有生成 route 文件或 route diff 产物，Agent manifest 也没有强类型
`routes` 字段。

## 架构图

![Topic 路由事实源与校验边界](../../assets/topic-routing-hybrid-sync.zh-CN.svg)

## 加载与校验链路

```text
config/eva.yaml
  -> 解析 ConfigRoots.route_file
  -> 使用 config/schemas/routes.schema.json 校验 route YAML
  -> eva-config::load_routes -> RouteConfig
  -> 加载 Agent manifest
  -> validate_project_config -> validate_route_agents
  -> RuntimeBuilder / basic::subscription_table
  -> eva-scheduler::SubscriptionTable
```

`load_project_config` 先执行 schema 校验，再进行强类型加载；随后把 route table
和 Agent manifest 放入同一个 `ProjectConfig`，并校验跨文件 target 引用。basic
runtime 把每个 `RouteRule` 转换为 Scheduler 对应的 `RoutingRule`。

所有阶段都不会把 YAML 写回磁盘。

## Route Schema

已实现的 route 形态如下：

```yaml
routes:
  - pattern: /sys/route-a
    delivery: fanout
    agents:
      - agent-a
```

| 字段 | 已实现规则 |
| --- | --- |
| `routes` | 必填，并且强类型加载后必须至少包含一条 route |
| `pattern` | 必填，并解析为 `TopicPattern` |
| `delivery` | 必填，只接受 `fanout` 和 `compete` |
| `agents` | 必填，并且必须至少包含一个合法 `AgentId` |
| 额外 route 字段 | 由 JSON Schema 拒绝 |

`TopicPattern` 支持精确 segment、匹配一个 segment 的 `*`，以及匹配零个或
多个尾部 segment 的最终 `**`。Pattern 必须以 `/` 开头，不能包含空 segment，
也不能以 `/` 结尾。

## 已存在的校验

`eva config validate` 运行与 runtime 命令相同的项目 loader。对 route 已实现的
校验包括：

- route 和 Agent 文件符合各自 JSON Schema；
- route table 非空；
- 每个 pattern 都能成功解析；
- delivery 为 `fanout` 或 `compete`；
- 每条 route 至少包含一个语法合法的 Agent ID；
- 每个目标 Agent ID 都存在于已加载的 Agent manifest 中。

Agent manifest 还会分别校验 ID 唯一、parent/child 引用合法、Lua 脚本存在、
subscription 与 emit pattern 合法，以及 provider/capability 引用合法。

当前 target 存在性校验包含 disabled Agent manifest，不要求目标
`enabled: true`。

## 尚不存在的校验

当前校验不会：

- 要求 route pattern 被目标 Agent 的 subscription 覆盖；
- 拒绝没有对应全局 route 的 subscription；
- 比较 route delivery 与 Agent permission；
- 从 `parent`、`children`、目录嵌套或 subscription 推导 route；
- 把重复或重叠 route pattern 判定为冲突；
- 让精确 pattern 优先于通配 pattern；
- 校验 priority、fallback、consumer group 或负载均衡 policy；
- 生成 warning、proposal、generated route 或 route diff。

因此配置 review 时要明确：`config validate` 成功只证明当前实现的 schema、
强类型和引用校验通过，并不证明 subscription 语义覆盖完整。

## 运行时路由语义

Scheduler 保持 route 文件顺序，并展开所有命中规则。

1. 显式 `EventTarget::Agent` 绕过 Topic route 展开。
2. 否则按源文件顺序处理所有匹配该 Topic 的 route pattern。
3. `fanout` 投递给每条命中规则列出的全部 Agent。
4. `compete` 当前只投递给每条命中规则列出的第一个 Agent。
5. 没有规则命中时返回结构化 not-found 错误。

系统不存在隐式的“最具体规则胜出”。精确 route 和通配 route 可以同时为同一
事件产生投递。

## Agent Subscriptions

`subscriptions` 仍是有用的 Agent metadata：

- `eva-config` 把每个值解析为 `TopicPattern`；
- project inspect 和 `eva agent status` 可以展示这些声明。

Scheduler 不会从这些声明构建 `SubscriptionTable`。因此 Agent 可以声明一个
subscription 却收不到事件，route 也可以指向声明未覆盖该 pattern 的 Agent。
最终 runtime 投递结果仍只来自 route 文件。

## CLI 边界

已经实现的配置命令是：

```text
eva config validate [--project <path>] [--output text|json]
```

`eva inspect config` 可以显示已加载项目的配置摘要，但不是 Scheduler effective
route dump。

以下命令不存在：

```text
eva config routes sync --check
eva config routes sync --write
eva config routes preview
eva config routes dump-effective
```

当前也没有 `RouteProposalBuilder`、`.eva/generated/routes/` 或
`.eva/reports/config/routes-diff.json` 实现。

## 变更与 Reload 边界

Route 变更是人工配置编辑：

1. 编辑 `config/routes/topics.yaml`，或 `config.route_file` 选中的文件。
2. 运行 `eva config validate`。
3. 重建或重启消费该配置的 runtime/command，使其加载新的 `ProjectConfig` 并
   构建新的 `SubscriptionTable`。

修改 YAML 不会改变已经构建的 table。Agent reload 和 daemon reload-plan
generation evidence 都不会重新读取、替换或写入 route 文件。当前没有与该配置
接线的 file watcher、自动同步或原子 live RouteTable replacement。

## 决策记录

| 决策 | 当前结论 |
| --- | --- |
| Runtime route 事实源 | `config.route_file` 选中的文件，通常为 `config/routes/topics.yaml` |
| Agent subscription 作用 | 已校验的声明，以及 inspect/status metadata |
| Route mutation | 只允许人工编辑 |
| CLI 支持 | 只读 `config validate`；没有 sync、preview、write 或 effective dump 命令 |
| Runtime 更新 | 显式重建/重启；没有自动热替换 |
| Compete 选择 | 命中规则中列出的第一个 Agent |

## 总结

Eva-CLI 保持 Topic 投递显式：route 文件决定谁接收事件，Agent subscription
只描述 Agent，不改变这项决定。校验保护语法、强类型值和 target 引用，但不会
合成 route，也不会证明 subscription coverage。
