# eva-cli/src / CLI 源码边界

更新时间：2026-07-03

本目录承载 `eva-cli` 的 V0.3 命令实现和后续命令边界。当前已经具备最小开发闭环：环境诊断、配置校验、系统摘要、no-op runtime 接入和稳定 exit code。

## 文件职责

| 文件 | 当前职责 | 当前状态 | 下一步 |
| --- | --- | --- | --- |
| `lib.rs` | 暴露 CLI 模块并提供 `eva_cli::run()` 顶层入口。 | 完成 V0.3 入口。 | 保持薄入口，不放业务解析。 |
| `run.rs` | 命令解析、通用参数、文本/JSON formatter、exit code、`config validate`、`run`。 | 完成 V0.3。 | V0.4 为 `run` 接真实 runtime event loop。 |
| `doctor.rs` | `eva doctor` 检查 workspace、配置根、schema、Lua host crate 边界、no-op runtime builder 和外部 adapter 声明。 | 完成 V0.3。 | V1.1 后增加 adapter/MCP probe。 |
| `inspect.rs` | 从 `ProjectConfig` 和 `RuntimeSummary` 构造 inspect 报告，输出 agents、adapters、capabilities、routes、policy domains 和 services。 | 完成 V0.3。 | 后续拆分真正的子视图输出。 |
| `emit.rs` | 事件发射命令占位。 | 边界保留。 | V0.4 接 typed ingress event 和 EventBus。 |
| `agent.rs` | Agent 命令占位。 | 边界保留。 | V0.4/V0.5 接 list、status、cancel。 |
| `adapter.rs` | Adapter 命令占位。 | 边界保留。 | V1.1 接 list、probe、route explain。 |
| `capability.rs` | Capability 命令占位。 | 边界保留。 | V0.4/V1.1 接 list、inspect、dry-run invoke。 |

## V0.3 命令行为

`run.rs` 不依赖外部 CLI parser crate，当前使用标准库手写解析，避免在 V0.3 为窄闭环引入新依赖。支持命令如下：

- `doctor [--project <path>] [--output text|json]`
- `config validate [--project <path>] [--output text|json]`
- `inspect [all|config|runtime|routes|policy|agents|adapters|capabilities] [--project <path>] [--output text|json]`
- `run [--project <path>] [--output text|json]`

`inspect` 的子视图名在 V0.3 仅用于兼容命令表面，输出仍是综合报告。这样可以让用户先稳定调用命令，而不提前冻结多个 JSON schema。

## 输出实现

- 文本输出面向人工诊断，先显示结论，再显示路径、数量、服务摘要和建议。
- JSON 输出使用统一 envelope，成功路径含 `ok`、`command`、`exit_code`、`data`、`trace`。
- 错误路径含 `ok`、`command`、`exit_code`、`error`、`trace`。
- `EvaError` 的 `kind`、`message`、`retryable`、`provider_code` 和 context 会被保留到 JSON 错误对象。
- `TraceFields` 当前只写入 CLI span，后续 runtime 接入后继续扩展。

## Exit code 映射

| ErrorKind / 情况 | Exit code | 说明 |
| --- | --- | --- |
| 成功 | `0` | 正常完成。 |
| `Internal` | `1` | 内部错误或输出错误。 |
| `InvalidArgument` / `NotFound` / `Conflict` | `2` | 配置、路径、manifest 或 route 问题。 |
| `PermissionDenied` | `3` | policy 拒绝。 |
| `Timeout` / `Unavailable` / `Unsupported` | `4` | runtime 或当前版本能力不可用。 |
| 预留外部能力不可用 | `5` | V0.3 暂不返回。 |
| 命令解析失败 | `64` | 未知命令、未知选项、缺少参数值。 |

## 测试入口

| 测试命令 | 覆盖目标 |
| --- | --- |
| `cargo test -p eva-cli` | CLI 单元测试和 doctor 检查。 |
| `cargo run -- doctor --output json` | 验证 doctor JSON envelope。 |
| `cargo run -- config validate --output json` | 验证配置校验 JSON envelope。 |
| `cargo run -- inspect runtime --output json` | 验证 inspect 与 no-op runtime summary。 |

## 维护要求

- 新增命令时先决定是否需要真实副作用；只读命令可以留在 CLI 层，副作用命令必须通过 `eva-runtime`。
- 新增 JSON 字段要保持向后兼容；删除或重命名字段应推迟到明确版本边界。
- 不要在 CLI 层重新解析 YAML 结构，配置事实源是 `eva-config`。
- 不要在 CLI 层直接 probe 外部 provider，V1.1 后通过 AdapterRuntime 实现。
