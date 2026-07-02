# eva-cli / 命令行入口

更新时间：2026-07-02

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-cli` 负责 Eva-CLI 的用户入口、命令解析、结构化输出和退出码映射。它可以调用 `eva-config`、`eva-policy`、`eva-observability` 和 `eva-runtime` 暴露的服务，但不拥有核心运行时状态，不直接读写 Agent 内部队列，不绕过 runtime 调用 Adapter、Lua、MCP 或硬件能力。

## 当前模块功能说明

| 功能域 | 当前状态 | 目标行为 |
| --- | --- | --- |
| `run` | 骨架 | 作为默认入口调用 runtime composition root，后续承载 `eva run` 和示例启动。 |
| `doctor` 入口 | 未实现 | 检查 workspace、配置文件、schema、数据目录、权限策略和可选外部工具。 |
| `config validate` | 未实现 | 调用 `eva-config::load_project_config` 和 `validate_project_config`，输出 human/json 两种诊断。 |
| `inspect` | 骨架 | 展示 effective config、routes、policy summary、runtime status，默认不输出敏感信息。 |
| `emit` | 骨架 | 构造受校验的 ingress event，后续通过 runtime/eventbus 发布。 |
| `agent` | 骨架 | 查询 Agent 列表、状态、mailbox 统计、任务状态和取消请求。 |
| `adapter` | 骨架 | 查询 Adapter manifest、probe 状态、transport 能力和 policy 限制。 |
| `capability` | 骨架 | 列出 capability、provider、schema、可调用性和路由结果。 |
| 输出契约 | 未实现 | 统一 `{ok,data,error,trace}` JSON envelope，错误来自 `EvaError`。 |
| 退出码 | 未实现 | 成功为 0，配置错误、权限拒绝、运行时错误、内部错误使用稳定枚举映射。 |

## 模块边界

`eva-cli` 做：

- 解析命令行参数和环境变量。
- 把配置、policy、runtime 返回值转成稳定输出。
- 维护用户可读诊断和机器可读 JSON。
- 执行前做 dry-run、plan、confirm 的用户交互门面。

`eva-cli` 不做：

- 不保存 runtime generation、mailbox、event log 或 Agent state。
- 不直接执行 Lua、shell、MCP、HTTP、硬件 I/O。
- 不重新实现配置解析、Topic 匹配或权限收紧逻辑。
- 不在输出中泄露 secret、token、payload 大文本或 provider 私有错误。

## 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V0.3 | 定义 command tree：`doctor`、`config validate`、`inspect`、`run`。 | 现有 `src/*.rs` 骨架 | `eva --help` 和子命令 help 可用。 |
| 2 | V0.3 | 增加输出 envelope、错误转 exit code、human/json formatter。 | `eva-core::EvaError`、`eva-observability::TraceFields` | 错误输出可 snapshot 测试。 |
| 3 | V0.3 | 实现 `config validate`，串联 `load_project_config` 和跨文件一致性检查。 | `eva-config` V0.2 | 示例配置成功，缺失脚本/重复 ID 有明确错误。 |
| 4 | V0.3 | 实现 `doctor`，检查 Rust workspace、配置目录、数据目录和只读权限。 | `eva-config`、标准库 | 不启动 runtime 也能诊断环境。 |
| 5 | V0.3 | 实现 `inspect config/routes/policy`，默认脱敏。 | `eva-config`、`eva-policy` | 输出稳定字段，支持 `--output json`。 |
| 6 | V0.3 | 接入 no-op runtime builder，用于 `inspect runtime`。 | `eva-runtime` V0.3 | 可从 validated config 构造空运行时摘要。 |
| 7 | V0.4 | 实现 `run --example basic` 和 `emit`，进入最小事件闭环。 | EventBus、Scheduler、Agent、Lua Host | `examples/basic/` 可端到端运行。 |
| 8 | V0.5 | 增加 `task status/logs/cancel`，统一长任务查询。 | AgentRuntime、EventLog、AuditSink | 长任务可观察、可取消。 |
| 9 | V1.1+ | 增加 `adapter probe`、`mcp list`、`discovery scan` 等扩展命令。 | Adapter/MCP/Discovery | 外部能力只展示候选和受控 handle。 |

## 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 公共 CLI 入口和模块导出 | 骨架，`run()` 委派到 `run::run()` | 引入命令解析入口和测试。 |
| `src/run.rs` | 启动 runtime 或示例 | 仅引用 runtime 边界常量 | 实现 `eva run` 参数、dry-run、runtime builder 调用。 |
| `src/inspect.rs` | 检查配置和运行时状态 | `RESPONSIBILITY` 占位 | 定义 inspect 子命令和 JSON 输出模型。 |
| `src/emit.rs` | 发布受校验 ingress event | `RESPONSIBILITY` 占位 | 复用 `eva-core::Event` 构造器，接 runtime eventbus。 |
| `src/agent.rs` | Agent 相关命令 | `RESPONSIBILITY` 占位 | 实现 `agent list/status/cancel` 命令模型。 |
| `src/adapter.rs` | Adapter 相关命令 | `RESPONSIBILITY` 占位 | 实现 `adapter list/probe` 输出契约。 |
| `src/capability.rs` | Capability 相关命令 | `RESPONSIBILITY` 占位 | 实现 `capability list/inspect/invoke --dry-run`。 |
| `src/README.md` | 源码目录说明 | 简略 | 同步文件职责和版本进度。 |
| CLI 集成测试 | 命令输出和退出码 | 未开始 | 先覆盖 `config validate --output json`。 |

## 验证计划

| 阶段 | 命令 | 目标 |
| --- | --- | --- |
| V0.3 单模块 | `cargo test -p eva-cli` | 验证 parser、formatter、exit code。 |
| V0.3 集成 | `cargo run -- config validate --output json` | 示例配置能输出稳定 JSON。 |
| V0.4 端到端 | `cargo run -- run --example basic` | CLI 到 runtime 事件闭环可运行。 |
| 回归 | `cargo test --workspace` | 下游接入不破坏契约模块。 |

## English

`eva-cli` owns command parsing, user-facing output, and exit-code mapping. It delegates configuration, policy, runtime, and execution work to lower crates and must not own runtime state or bypass policy-controlled service boundaries.
