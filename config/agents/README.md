# Agents / 智能体配置

## 中文

每个 Agent 使用独立目录维护，至少包含 `agent.yaml` 和 `main.lua`。固定职责、禁止行为和输出约束写入同目录的 `constraints.md`，并由 `agent.yaml` 显式引用。

运行时父子关系、订阅关系和权限关系以 `agent.yaml` 中的 `id`、`parent`、`children`、`subscriptions` 和 `permissions.emit` 为准，不依赖目录嵌套自动推断。

## English

Each Agent is maintained in its own directory with at least `agent.yaml` and `main.lua`. Stable responsibilities, forbidden behavior, and output constraints live in `constraints.md` and are referenced explicitly by `agent.yaml`.

Runtime parent-child relationships, subscriptions, and permissions are defined by `id`, `parent`, `children`, `subscriptions`, and `permissions.emit` in `agent.yaml`; they are not inferred from directory nesting alone.
