# Agents / 智能体配置

## 中文

每个 Agent 使用独立目录维护，至少包含 `agent.yaml` 和 `main.lua`。固定职责、禁止行为和输出约束写入同目录的 `constraints.md`，并由 `agent.yaml` 显式引用。

推荐目录名与 Agent ID 一致：

```text
config/agents/
  root-agent/
  agent-a/
  agent-a11/
  agent-a12/
```

运行时父子关系、订阅关系、Topic 路由和权限关系以 `agent.yaml` 中的 `id`、`parent`、`children`、`subscriptions` 和 `permissions.emit` 为准，不依赖目录嵌套自动推断。`/sys/route-a/route-aa` 这类 Topic 只应出现在 Agent 配置或 `config/routes/topics.yaml` 中，不应作为 Agent 存放目录。

## English

Each Agent is maintained in its own directory with at least `agent.yaml` and `main.lua`. Stable responsibilities, forbidden behavior, and output constraints live in `constraints.md` and are referenced explicitly by `agent.yaml`.

Prefer directory names that match Agent IDs:

```text
config/agents/
  root-agent/
  agent-a/
  agent-a11/
  agent-a12/
```

Runtime parent-child relationships, subscriptions, Topic routes, and permissions are defined by `id`, `parent`, `children`, `subscriptions`, and `permissions.emit` in `agent.yaml`; they are not inferred from directory nesting alone. Topics such as `/sys/route-a/route-aa` should appear only in Agent config or `config/routes/topics.yaml`, not as Agent storage directories.
