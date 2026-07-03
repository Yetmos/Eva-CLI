# eva-cli / 命令行入口

更新时间：2026-07-03

`eva-cli` 负责命令解析、文本/JSON 输出、trace 字段和 exit code 映射。CLI 不直接持有运行时状态；V0.4 的真实事件闭环通过 `eva-runtime` 进入。

## 当前命令

| 命令 | 当前行为 |
| --- | --- |
| `eva doctor` | 检查 workspace、配置根、schema、Lua host crate 边界、runtime builder。 |
| `eva config validate` | 加载 `eva.yaml`、Agent/Adapter/Capability manifest、policy、routes，并输出摘要。 |
| `eva inspect` | 输出 agents、adapters、capabilities、routes、policy domains 和 runtime service summary。 |
| `eva run --example basic` | 执行 V0.4 in-memory basic event loop。 |

## `eva run --example basic`

示例命令：

```powershell
cargo run -- run --example basic --output json
```

CLI 会将 `--example basic` 解析为 `<project>/examples/basic`，加载该目录的 `config/eva.yaml`，用 `RuntimeBuilder::in_memory_v04()` 构造 runtime，然后调用 `Runtime::run_basic`。

JSON 输出包含：

- `receipt`：EventBus publish receipt。
- `deliveries`：scheduler 产生的 Agent 投递计划。
- `agent_runs`：AgentRuntime handler 结果。
- `lua_results`：受控 Lua `on_event` 返回结果。
- `capability_response`：builtin capability 调用结果。
- `audit`：关键 handoff 摘要。

## 输出契约

成功 JSON 使用统一 envelope：`ok`、`command`、`exit_code`、`data`、`trace`。错误 JSON 使用：`ok`、`command`、`exit_code`、`error`、`trace`。文本模式保持人类可读摘要。

## Exit Code

| Code | 含义 |
| --- | --- |
| `0` | 成功。 |
| `1` | 内部错误。 |
| `2` | 配置、路径、manifest、route 或 schema 问题。 |
| `3` | policy 拒绝。 |
| `4` | runtime 当前不可用或能力未实现。 |
| `5` | 预留给外部 capability unavailable。 |
| `64` | 命令用法错误。 |

## 验证

```powershell
cargo test -p eva-cli
cargo run -- doctor --output json
cargo run -- config validate --output json
cargo run -- inspect runtime --output json
cargo run -- run --example basic --output json
```

已覆盖：config validate JSON、inspect text、unknown command、JSON escaping、basic run JSON 成功路径、doctor sample project。
