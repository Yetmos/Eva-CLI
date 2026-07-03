# eva-cli / 命令行入口

更新时间：2026-07-03

`eva-cli` 负责命令解析、文本/JSON 输出、trace 字段和 exit code 映射。V1.2 在 V1.0 basic runtime 和 V1.1 external capability surface 之上新增 `memory context`，用于输出 request-scoped private/global memory、knowledge search 和 Lua context 摘要。CLI 不启动后台 daemon；`run --example basic` 同步执行后，会把最新 task report 写入 `.eva/tasks`，供 `task status/logs/cancel` 跨命令读取。

## 当前命令

| 命令 | 当前行为 |
| --- | --- |
| `eva version` / `eva --version` | 输出 V1.0 core 版本、runtime mode 和稳定命令契约。 |
| `eva doctor` | 检查 workspace、配置根、schema、Lua host crate 边界、runtime builder。 |
| `eva config validate` | 加载 `eva.yaml`、Agent/Adapter/Capability manifest、policy、routes，并输出摘要。 |
| `eva inspect` | 输出 agents、adapters、capabilities、routes、policy domains 和 runtime service summary。 |
| `eva run --example basic` | 执行 V1.0 in-memory basic event loop，并写入本地 task report。 |
| `eva task status` | 读取 `.eva/tasks` 中最新或指定 task 的状态、attempts、retry、取消和 dead-letter 摘要。 |
| `eva task logs` | 读取 task logs。 |
| `eva task cancel` | 对未终态 task 写入取消标记；对已终态 task 记录 cancel request 但不改变终态。 |
| `eva memory context` | 为单个 Agent 构造 V1.2 request context，输出 private memory、global memory、knowledge、Lua snapshot 和 audit。 |

## `eva run --example basic`

示例命令：

```powershell
cargo run -- run --example basic --output json
cargo run -- run --example basic --timeout-ms 0 --replay-dead-letters --output json
cargo run -- run --example basic --cancel --output json
```

常用选项：

| 选项 | 含义 |
| --- | --- |
| `--task-id <id>` | 指定 request/task id；默认 `req-basic-1`。 |
| `--timeout-ms <ms>` | 设置 Agent handler timeout budget；`0` 会触发 V1.0 timeout 诊断路径。 |
| `--retry-attempts <n>` | 设置 retry 上限；当前 basic handler 只在结构化 retryable 错误时重试。 |
| `--cancel` | 在 handler 前模拟取消请求，生成 cancelled task。 |
| `--replay-dead-letters` | 对当前 run 的 dead-letter 事件生成 replay receipt 摘要。 |

JSON 输出在 V0.4 字段基础上新增：

- `task`：task id、status、attempts、retry policy、cancellation、logs、dead_letters、replayed_events。
- `agent_runs[].attempts`：Agent handler 尝试次数。
- `lua_generation`：当前 basic run 的 generation id 和脚本数量。

## `eva task ...`

`task` 命令读取 `<project>/.eva/tasks`：

```powershell
cargo run -- task status --output json
cargo run -- task logs --task req-basic-1 --output json
cargo run -- task cancel --task req-basic-1 --reason "manual stop" --output json
```

V1.0 的 task 记录是本地诊断文件，不是 durable task database。目录已在 `.gitignore` 中排除。

## 输出契约

成功 JSON 使用统一 envelope：`ok`、`command`、`exit_code`、`data`、`trace`。错误 JSON 使用：`ok`、`command`、`exit_code`、`error`、`trace`。文本模式保持人类可读摘要。

## Exit Code

| Code | 含义 |
| --- | --- |
| `0` | 成功。 |
| `1` | 内部错误。 |
| `2` | 配置、路径、manifest、route、schema 或 task state 问题。 |
| `3` | policy 拒绝。 |
| `4` | runtime 当前不可用或能力未实现。 |
| `5` | 预留给外部 capability unavailable。 |
| `64` | 命令用法错误。 |

## 验证

```powershell
cargo test -p eva-cli
cargo run -- --version
cargo run -- version --output json
cargo run -- doctor --output json
cargo run -- config validate --output json
cargo run -- inspect runtime --output json
cargo run -- run --example basic --output json
cargo run -- task status --output json
cargo run -- task logs --output json
cargo run -- adapter list --output json
cargo run -- adapter probe --adapter github-mcp --output json
cargo run -- mcp probe --adapter github-mcp --tool list_issues --output json
cargo run -- skill run --skill code-review --input '{"scope":"current_diff"}' --output json
cargo run -- discovery scan --output json
cargo run -- memory context --agent root-agent --query context --private-limit 1 --output json
```

## V1.1 External Capability Commands

V1.1 adds CLI coverage for the external capability ecosystem while preserving the V1.0 basic runtime path:

- `eva adapter list`: lists authorized Adapter handles derived from project manifests.
- `eva adapter probe --adapter <id>`: reports side-effect-free health, transport, and capability details for one handle.
- `eva adapter probe --capability <name> [--provider <id>]`: exercises AdapterRouter selection without invoking an external provider.
- `eva mcp list`: lists MCP transport adapters and their tool allowlists.
- `eva mcp probe --adapter <id> --tool <name>`: reports whether the tool is allowlisted; blocked probes still return a diagnostic success envelope because no provider was invoked.
- `eva skill list`: lists controlled workflow skill adapters and runtime gates.
- `eva skill run --skill <id> --input <json>`: returns a controlled Skill envelope and audit trail without starting Codex/OMX workflow execution.
- `eva discovery scan`: returns trusted candidates with `handle_granted=false` to prove discovery is not authorization.

The commands use the same success/error JSON envelopes, trace fields, and exit-code rules as V1.0 commands. V1.1 does not start real stdio/http/MCP server processes; it proves the policy-shaped control surface first.

## V1.2 Memory Context Command

`eva memory context` is the V1.2 CLI smoke for `eva-memory` and `eva-lua-host`:

```powershell
cargo run -- memory context --agent root-agent --query context --private-limit 1 --output json
```

Important options:

| Option | Meaning |
| --- | --- |
| `--agent <id>` | Agent whose private memory may enter the context. Defaults to `root-agent`. |
| `--query <text>` | Knowledge search query. Defaults to `memory`. |
| `--request-id <id>` | Request id attached to seeded context records. Defaults to `req-memory-1`. |
| `--private-limit <n>` | Maximum private memory records returned. |
| `--global-limit <n>` | Maximum global memory records returned. |
| `--knowledge-limit <n>` | Maximum knowledge search results returned. |

JSON output uses the normal success envelope and contains `memory`,
`global_memory`, `knowledge`, `lua_context`, and `audit`. The command uses an
in-memory demonstration context derived from the project configuration; durable
memory storage remains later scope.

已覆盖：version text/JSON、config validate JSON、inspect text、unknown command、JSON escaping、basic run JSON、cancelled basic run、task status/logs/cancel、doctor sample project。
