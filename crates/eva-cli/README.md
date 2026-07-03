# eva-cli / 命令行入口

更新时间：2026-07-03

`eva-cli` 是 Eva-CLI 的用户面边界，负责命令解析、参数校验、文本/JSON 输出、trace 字段透传和 exit code 映射。它不持有 runtime 状态，不直接执行 EventBus、Agent、Lua、Adapter、MCP 或硬件 I/O；这些副作用必须等对应 runtime 服务实现后再通过组合根进入。

## V0.3 已实现范围

| 命令 | 当前行为 | 副作用边界 |
| --- | --- | --- |
| `eva doctor` | 检查 workspace 根、`Cargo.toml`、`config/eva.yaml`、配置根、schema 文件、Lua host crate 边界和 no-op runtime builder。 | 只读文件系统，不启动 runtime。 |
| `eva config validate` | 调用 `eva-config::load_project_config`，加载 `eva.yaml`、Agent/Adapter/Capability manifest、policy document 和 routes，并输出配置摘要。 | 只做加载与跨文件校验。 |
| `eva inspect` | 输出 agents、adapters、capabilities、routes、policy domains 和 no-op runtime service summary。 | 只读 validated config 和 `RuntimeSummary`。 |
| `eva inspect config/routes/policy/runtime/agents/adapters/capabilities` | 接受常用 inspect 子视图别名；V0.3 仍返回同一个综合摘要，避免过早承诺分裂输出结构。 | 只读。 |
| `eva run` | 先加载配置并构造 no-op runtime，再返回 `Unsupported`，提示 V0.4 才会接真实事件循环。 | 不启动 EventBus、Agent 或 Lua。 |

`emit`、`agent`、`adapter`、`capability` 模块仍保留为后续命令边界，目前不暴露稳定用户命令。

## 全局参数

| 参数 | 支持命令 | 说明 |
| --- | --- | --- |
| `--project <path>` / `--project-root <path>` / `-p <path>` | `doctor`、`config validate`、`inspect`、`run` | 指向 Eva workspace 根目录；默认使用当前工作目录。 |
| `--output text` / `--output human` / `-o text` | `doctor`、`config validate`、`inspect`、`run` | 输出人类可读文本。 |
| `--output json` / `-o json` | `doctor`、`config validate`、`inspect`、`run` | 输出机器可读 JSON envelope。 |
| `--help` / `-h` | 顶层 | 输出 V0.3 命令帮助。 |

## 输出契约

文本模式先给结论，再列路径、数量、服务状态和建议。错误文本使用 `ERROR <command> [<kind>] <message>`，随后输出错误上下文和建议。

JSON 成功输出使用统一 envelope：`ok`、`command`、`exit_code`、`data`、`trace`。JSON 错误输出使用 `ok`、`command`、`exit_code`、`error`、`trace`，其中 `error` 包含 `kind`、`message`、`retryable`、`provider_code`、`context` 和 `suggestion`。

当前 trace 来源是 `eva-observability::TraceFields`，V0.3 先写入稳定 `span_id`，后续 runtime 事件闭环会继续补 `event_id`、`request_id`、`topic` 和 generation 字段。

## Exit code

| Code | 当前含义 | 来源 |
| --- | --- | --- |
| `0` | 成功。 | `doctor` 无 error check、`config validate` 成功、`inspect` 成功。 |
| `1` | 内部错误。 | 输出失败或未预期内部错误。 |
| `2` | 配置、路径、manifest、routes 或 schema 相关错误。 | `InvalidArgument`、`NotFound`、`Conflict`。 |
| `3` | policy 拒绝。 | `PermissionDenied`。 |
| `4` | runtime 当前不可用或能力尚未实现。 | `Timeout`、`Unavailable`、`Unsupported`，包括 V0.3 的 `eva run`。 |
| `5` | 预留给外部能力不可用。 | V0.3 暂不返回，V1.1 Adapter/MCP probe 接入后启用。 |
| `64` | 命令用法错误。 | 未知命令、未知选项、缺少参数值。 |

## 与下层 crate 的关系

- `eva-config` 负责 YAML、manifest、routes、policy document 加载与跨文件校验。
- `eva-core` 负责 `EvaError`、`ErrorKind` 和基础契约类型。
- `eva-observability` 提供 `TraceFields` 和 `SpanId`，CLI 只做展示，不自定义观测协议。
- `eva-runtime` 提供 V0.3 no-op `RuntimeBuilder`、`RuntimeSummary` 和 service summary。

## 已覆盖测试

| 测试 | 覆盖内容 |
| --- | --- |
| `config_validate_json_succeeds_for_sample_project` | 样例 workspace 配置可被 CLI 加载并输出 JSON envelope。 |
| `inspect_text_reports_noop_runtime` | `inspect` 文本输出包含 no-op runtime 和 agent 摘要。 |
| `unknown_command_is_usage_error` | 未知命令返回 usage exit code。 |
| `json_string_escapes_control_characters` | 手写 JSON formatter 会转义引号、反斜杠和控制字符。 |
| `doctor_accepts_sample_project_with_only_v03_warnings` | `doctor` 对样例项目不产生 error，并保留外部 adapter 未 probe 的 V0.3 warning。 |

## 验证命令

| 命令 | 目标 |
| --- | --- |
| `cargo test -p eva-cli` | 验证 CLI parser、formatter、exit code 和 doctor/inspect 报告。 |
| `cargo run -- doctor --output json` | 验证环境诊断 JSON 输出。 |
| `cargo run -- config validate --output json` | 验证配置加载与诊断 JSON 输出。 |
| `cargo run -- inspect runtime --output json` | 验证 no-op runtime 摘要可经 CLI 读取。 |
| `cargo test --workspace` | 验证 CLI 接入不破坏其他 crate。 |

## 剩余限制

- `eva run` 在 V0.3 只验证配置和组合根，真实事件循环留给 V0.4。
- `inspect` 子视图参数目前只是别名过滤，输出仍是综合摘要。
- JSON formatter 为标准库手写实现，后续如引入正式序列化依赖，需要保持字段兼容。
- 外部 adapter、MCP、skill、hardware 只展示声明和 warning，不做 probe 或调用。
