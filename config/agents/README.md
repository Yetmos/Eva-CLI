# Agents / 智能体配置

## 中文

每个 Agent 使用独立目录，目录名应与 Agent ID 一致，并至少包含 `agent.yaml` 和 `main.lua`。

`constraints.md` 是可选的稳定行为约束文档。需要使用时，必须通过 `agent.yaml` 中的 `constraints.file` 显式引用；未引用的同名文件不会自动生效。

父子关系、订阅、局部路由意图和发布权限以 manifest 中的 `id`、`parent`、`children`、`subscriptions`、`routes` 和 `permissions.emit` 为准，不根据目录嵌套推断。生产 Topic 投递以 `config/routes/topics.yaml` 为事实源。

## English

Each Agent has its own directory. The directory name should match the Agent ID and contain at least `agent.yaml` and `main.lua`.

A `constraints.md` file is optional. When used, it must be referenced explicitly through `constraints.file` in `agent.yaml`; an unreferenced file is not loaded implicitly.

Parent-child relationships, subscriptions, local routing intent, and emit permissions come from `id`, `parent`, `children`, `subscriptions`, `routes`, and `permissions.emit` in the manifest, not directory nesting. `config/routes/topics.yaml` remains the source of truth for production Topic delivery.
