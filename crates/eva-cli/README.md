# eva-cli / 命令行入口

更新时间：2026-07-04

`eva-cli` 负责命令解析、文本/JSON 输出、trace 字段和 exit code 映射。当前命令面覆盖 V1.0 core runtime、V1.1 外部能力诊断、V1.2 request-scoped memory/knowledge context，以及 V1.3 plan-first 硬件接入诊断。

CLI 不启动后台 daemon；`run --example basic` 同步执行后，会把最新 task report 写入 `.eva/tasks`，供 `task status/logs/cancel` 跨命令读取。外部能力、记忆上下文和硬件命令当前都以可验证诊断 surface 为主，不打开真实 stdio/http/MCP server 或 raw hardware I/O。

## 当前命令

| 命令 | 当前行为 |
| --- | --- |
| `eva version` / `eva --version` | 输出 V1.3 release label、runtime contract 和稳定命令契约。 |
| `eva doctor` | 检查 workspace、配置根、schema、Lua host crate 边界、runtime builder。 |
| `eva config validate` | 加载 `eva.yaml`、Agent/Adapter/Capability manifest、policy、routes，并输出摘要。 |
| `eva inspect` | 输出 agents、adapters、capabilities、routes、policy domains 和 runtime service summary。 |
| `eva run --example basic` | 执行 V1.0 in-memory basic event loop，并写入本地 task report。 |
| `eva task status` | 读取 `.eva/tasks` 中最新或指定 task 的状态、attempts、retry、取消和 dead-letter 摘要。 |
| `eva task logs` | 读取 task logs。 |
| `eva task cancel` | 对未终态 task 写入取消标记；对已终态 task 记录 cancel request 但不改变终态。 |
| `eva adapter list/probe` | 列出和 probe 已授权 Adapter handle，不启动真实 provider。 |
| `eva mcp list/probe` | 列出和 probe MCP allowlist tool，不启动真实 MCP server。 |
| `eva skill list/run` | 返回受控 workflow skill envelope，不启动 Codex/OMX workflow runner。 |
| `eva discovery scan` | 返回 discovery candidate，明确 discovery 不授予 runtime handle。 |
| `eva memory context` | 为单个 Agent 构造 V1.2 request context，输出 private/global memory、knowledge、Lua snapshot 和 audit。 |
| `eva hardware list/probe/bind` | 发现、probe 并计划硬件绑定；高风险动作 plan-first，V1.3 不打开 raw I/O。 |

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
| `--timeout-ms <ms>` | 设置 Agent handler timeout budget；`0` 会触发 timeout 诊断路径。 |
| `--retry-attempts <n>` | 设置 retry 上限；当前 basic handler 只在结构化 retryable 错误时重试。 |
| `--cancel` | 在 handler 前模拟取消请求，生成 cancelled task。 |
| `--replay-dead-letters` | 对当前 run 的 dead-letter 事件生成 replay receipt 摘要。 |

## `eva task ...`

`task` 命令读取 `<project>/.eva/tasks`：

```powershell
cargo run -- task status --output json
cargo run -- task logs --task req-basic-1 --output json
cargo run -- task cancel --task req-basic-1 --reason "manual stop" --output json
```

V1.0 的 task 记录是本地诊断文件，不是 durable task database。目录已在 `.gitignore` 中排除。

## V1.1 External Capability Commands

V1.1 增加外部能力生态命令，同时保留 V1.0 basic runtime 路径：

- `eva adapter list`：列出从项目 manifest 派生的授权 Adapter handle。
- `eva adapter probe --adapter <id>`：返回 side-effect-free health、transport 和 capability 详情。
- `eva adapter probe --capability <name> [--provider <id>]`：验证 AdapterRouter 选择，不调用外部 provider。
- `eva mcp list`：列出 MCP transport adapters 和 tool allowlist。
- `eva mcp probe --adapter <id> --tool <name>`：报告 tool 是否在 allowlist 中；blocked probe 也返回诊断 envelope，因为未调用 provider。
- `eva skill list`：列出受控 workflow skill adapters 和 runtime gates。
- `eva skill run --skill <id> --input <json>`：返回受控 Skill envelope 和 audit trail，不启动 workflow execution。
- `eva discovery scan`：返回 trusted candidates，并用 `handle_granted=false` 证明 discovery 不是授权。

## V1.2 Memory Context Command

`eva memory context` 是 V1.2 的 `eva-memory` 和 `eva-lua-host` smoke：

```powershell
cargo run -- memory context --agent root-agent --query context --private-limit 1 --output json
```

| Option | Meaning |
| --- | --- |
| `--agent <id>` | Agent whose private memory may enter the context. Defaults to `root-agent`. |
| `--query <text>` | Knowledge search query. Defaults to `memory`. |
| `--request-id <id>` | Request id attached to seeded context records. Defaults to `req-memory-1`. |
| `--private-limit <n>` | Maximum private memory records returned. |
| `--global-limit <n>` | Maximum global memory records returned. |
| `--knowledge-limit <n>` | Maximum knowledge search results returned. |

JSON output contains `memory`, `global_memory`, `knowledge`, `lua_context`, and `audit`. The command uses an in-memory demonstration context derived from project configuration; durable memory storage remains later scope.

## V1.3 Hardware Commands

V1.3 新增 `hardware list/probe/bind`，用于诊断 hardware Adapter manifest 和生成绑定计划：

```powershell
cargo run -- hardware list --output json
cargo run -- hardware probe --adapter scale-main --output json
cargo run -- hardware bind --adapter scale-main --output json
cargo run -- hardware bind --adapter scale-main --apply --output json
```

| 命令 | 行为 |
| --- | --- |
| `hardware list` | 加载项目配置，调用 `eva_hardware::discover_project_devices`，输出所有 hardware candidates。 |
| `hardware probe --adapter <id>` | 过滤单个 Adapter 的候选，仍不授予 handle。 |
| `hardware bind --adapter <id>` | 生成绑定计划、风险提示和 plan steps；disabled/rejected 设备返回 `blocked`。 |
| `hardware bind --apply` | V1.3 只校验逻辑计划并保留 plan-first 输出，不打开真实设备。 |

`scale-main` 默认 disabled，因此 JSON 中会看到：

- `trust: "rejected"`
- `health: "disconnected"`
- `handle_granted: false`
- `rejected_reason: "hardware adapter manifest is disabled"`
- `hardware bind` 的 `status: "blocked"`

这条命令面验证硬件身份、发现、绑定计划和风险提示，但不执行 USB、串口、BLE、网络或 vendor SDK I/O。

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
cargo run -- hardware list --output json
cargo run -- hardware probe --adapter scale-main --output json
cargo run -- hardware bind --adapter scale-main --output json
```

当前测试覆盖 version text/JSON、config validate JSON、inspect text、unknown command、JSON escaping、basic run JSON、cancelled basic run、task status/logs/cancel、doctor sample project、V1.1 external capability commands、V1.2 memory context 和 V1.3 hardware command JSON。
