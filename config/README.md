# Eva-CLI Configuration / Eva-CLI 配置

## 中文

配置模型、合并顺序和安全边界以[项目配置方案](../docs/zh-CN/operations/项目配置方案.md)为准。本目录是仓库内的项目配置根：人工维护格式为 YAML，启动前使用 JSON Schema 和 Rust 强类型加载器校验，运行时协议消息使用 JSON。

| 路径 | 职责 |
| --- | --- |
| `eva.yaml` | 全局 runtime、EventBus、Scheduler、状态、记忆、知识库和观测设置，以及拆分配置入口。 |
| `agents/` | Agent manifest、Lua 入口脚本和可选约束文档。 |
| `adapters/` | Codex CLI、Claude API、MCP、Skill 和硬件 Adapter manifest。 |
| `capabilities/` | Skill、MCP、Tool 和 Lua capability 声明。 |
| `policies/` | 沙箱、Adapter、MCP、硬件和记忆策略。 |
| `routes/topics.yaml` | Scheduler 使用的 Topic 路由事实源。 |
| `schemas/` | 配置文件的 JSON Schema。 |

配置不变量：

- Agent 目录至少包含 `agent.yaml` 和 `main.lua`。`constraints.md` 是可选文件；只有被 `agent.yaml` 的 `constraints.file` 显式引用时才会成为 Agent 配置的一部分，不能按文件名隐式加载。
- Capability 文件只声明入口、参数、权限和运行限制，不能绕过 Adapter manifest 或全局 policy 获得执行权。
- Policy 定义全局权限下限。Agent、Adapter 和请求级配置只能进一步收紧权限，不能放宽上层边界。
- Topic route 只描述事件投递，不承载业务逻辑。显式 `target` 优先于路由匹配，`**` 只能出现在 Topic pattern 的最后一段。
- Schema 必须与 Rust 强类型配置保持同步，并在启动前校验 YAML 解析结果。
- Hardware Adapter 使用 `transport: hardware`，声明 bus、match、identity、protocol、hotplug、driver 和 limits。支持的 driver kind 包括 `simulated`、`usb`、`serial`、`ble`、`socket` 和 `vendor_sdk`；仓库示例保持禁用并使用 simulator，全局边界由 `policies/hardware.yaml` 定义。
- Adapter 可选声明 `supervision.restart`（`none`、`on_failure`、`always`）和 `supervision.run_as`（`current`、`unix`、`windows`）；缺失时兼容旧清单并默认为 `none/current`。`max_attempts` 表示首次启动后的自动重启次数，真实重启与身份执行属于后续 runtime 任务。
- `credentials.vault` 只保存 `vault://...` 引用，并且目标环境变量必须同时出现在 `permissions.env`；当前 W3-L01 只校验、排序和传播引用，不执行取密。生产环境拒绝明文敏感字段、未白名单的 `env:` 引用和直接发送的 `vault://` header。
- 这里只保存环境变量名、权限边界和连接声明，不保存 API key、token 或用户密钥明文。

## English

The authoritative configuration model, merge order, and security boundaries are defined in [Project Configuration](../docs/en/operations/project-configuration.md). This directory is the repository's project configuration root: YAML is the human-maintained format, JSON Schema and strongly typed Rust loaders validate it before startup, and runtime protocol messages use JSON.

| Path | Ownership |
| --- | --- |
| `eva.yaml` | Global runtime, EventBus, Scheduler, state, memory, knowledge, and observability settings plus split-config pointers. |
| `agents/` | Agent manifests, Lua entry scripts, and optional constraint documents. |
| `adapters/` | Adapter manifests for Codex CLI, Claude API, MCP, Skills, and hardware. |
| `capabilities/` | Skill, MCP, Tool, and Lua capability declarations. |
| `policies/` | Sandbox, Adapter, MCP, hardware, and memory policies. |
| `routes/topics.yaml` | The Scheduler's authoritative Topic routing table. |
| `schemas/` | JSON Schemas for configuration files. |

Configuration invariants:

- An Agent directory contains at least `agent.yaml` and `main.lua`. A `constraints.md` file is optional and becomes part of the Agent configuration only when `agent.yaml` explicitly references it through `constraints.file`; filenames are never loaded implicitly.
- Capability files declare entry points, parameters, permissions, and runtime limits. They do not bypass Adapter manifests or global policy to gain execution authority.
- Policies define global permission floors. Agent, Adapter, and request-level configuration may only narrow those boundaries.
- Topic routes describe event delivery, not business logic. An explicit `target` takes precedence over route matching, and `**` may appear only as the final Topic-pattern segment.
- Schemas stay aligned with the strongly typed Rust configuration and validate parsed YAML before startup.
- Hardware Adapters use `transport: hardware` and declare bus, match, identity, protocol, hotplug, driver, and limits. Supported driver kinds include `simulated`, `usb`, `serial`, `ble`, `socket`, and `vendor_sdk`; the repository sample remains disabled and simulated, while `policies/hardware.yaml` defines the global boundary.
- Store environment variable names, permissions, and connection declarations here, never plaintext API keys, tokens, or user secrets.
- Adapters may declare `supervision.restart` (`none`, `on_failure`, or `always`) and `supervision.run_as` (`current`, `unix`, or `windows`); missing sections preserve legacy `none/current` defaults. `max_attempts` counts automatic restarts after the initial start; real restart and identity enforcement belong to later runtime tasks.
- `credentials.vault` stores only `vault://...` references, and each target environment variable must also be listed in `permissions.env`. W3-L01 validates, sorts, and propagates references but does not fetch secrets; production rejects plaintext sensitive fields, non-allowlisted `env:` references, and direct `vault://` headers.
