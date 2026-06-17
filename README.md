# Eva-CLI

Eva-CLI 当前处于架构方案整理阶段，仓库内主要内容是 `docs/` 下的设计文档，还不是可运行实现。文档与官网已迁移为英语 canonical source + 多语言可扩展结构。

官网：

- https://Eva-CLI.com
- https://www.Eva-CLI.com

官网源码维护在 [website/](website/)，文档维护在 [docs/](docs/)，后续 Rust 源码维护在 [src/](src/) 和 [crates/](crates/)。

## 仓库结构

```text
Eva-CLI/
  src/                 # 主程序源码
  crates/              # Rust workspace 子 crate
  docs/                # 架构文档与实现规范
  website/             # 官网源码
  examples/            # 示例和集成演示
  assets/              # 图片、图表等公共资源
  .github/workflows/   # CI、部署和自动化工作流
```

当前官网是零运行时依赖静态页面。GitHub Pages 工作流会先运行 `scripts/build-site-i18n.ps1` 生成本地化 HTML，再运行 `scripts/validate-i18n.ps1` 校验结构，然后把 `website/`、`docs/` 和 `assets/` 组合后发布。

## 架构总览

![Eva-CLI 总体架构](assets/eva-cli-architecture.svg)

这张图概括当前方案的主链路：入口与配置热加载进入 Rust 托管 Runtime，经过 Recoverable EventBus 和 Scheduler 投递到 Lua Agent；Lua 只在沙箱内处理业务逻辑，并通过 Rust Tool Layer、AdapterRegistry、MemoryService、KnowledgeService 和 HardwareAdapter 访问受控能力。

## 文档入口

默认文档入口：

- [English canonical docs](docs/en/README.md)
- [简体中文文档](docs/zh-CN/README.md)
- [文档维护入口](docs/README.md)

建议先按以下顺序阅读英文 canonical 文档：

1. [Architecture Overview](docs/en/architecture-overview.md)：先建立系统边界、核心模块和总体结论。
2. [Rust, Lua, and EventBus Scheduler](docs/en/rust-lua-eventbus-scheduler.md)：理解 Runtime、EventBus、Scheduler、Lua Agent 和 Topic 路由。
3. [Lua External Agent Adapter](docs/en/lua-external-agent-adapter.md)：理解外部 Agent、CLI、HTTP、MCP、Skill 如何通过 Adapter 接入。
4. [Lua Skill, MCP, and Tool Hot Reload](docs/en/lua-skill-mcp-tool-hot-reload.md)：理解 Tool、Lua Skill 和 MCP tool handler 如何下沉到 Lua 并热更新。
5. [Agent Memory and Knowledge Base](docs/en/agent-memory-knowledge-base.md)：理解 Agent 私有记忆、系统总记忆库、知识库和上下文构建边界。
6. [Agent Discovery](docs/en/agent-discovery.md)：理解项目配置、用户环境、MCP、Skill 和 Lua capability 如何被发现与注册。
7. [Hardware Hotplug](docs/en/hardware-hotplug.md)：理解 USB、串口、BLE、网络设备和厂商 SDK 设备如何通过 HardwareAdapter 接入并支持热插拔。
8. [Project Configuration](docs/en/project-configuration.md)：理解 YAML 配置、schema、policy、manifest 和热加载边界。
9. [Process-Level Upgrade](docs/en/process-level-upgrade.md)：理解 Supervisor、Runtime generation、blue-green、draining、恢复和回滚。
10. [Design Risk Review](docs/en/design-risk-review.md)：集中查看当前方案的纯设计风险、语义缺口和需要补强的设计面。

## 文档职责

| 文档 | 职责 |
| --- | --- |
| [Architecture Overview](docs/en/architecture-overview.md) | 总入口，统一系统目标、非目标、模块边界、运行链路、安全原则和待补设计。 |
| [Rust, Lua, and EventBus Scheduler](docs/en/rust-lua-eventbus-scheduler.md) | 定义 Rust Runtime、Lua Agent、EventBus、Topic、Scheduler、状态、一致性和热更新。 |
| [Lua External Agent Adapter](docs/en/lua-external-agent-adapter.md) | 定义 AdapterRegistry、AdapterRouter、McpAdapter、SkillAdapter、HardwareAdapter、stdio/http/eventbus/hardware 等外部能力接入。 |
| [Lua Skill, MCP, and Tool Hot Reload](docs/en/lua-skill-mcp-tool-hot-reload.md) | 定义 `lua_tool`、`lua_skill`、`lua_mcp_handler`、Lua Capability Runtime、host API、安全沙箱和 generation swap。 |
| [Agent Memory and Knowledge Base](docs/en/agent-memory-knowledge-base.md) | 定义 Agent 私有记忆、系统总记忆库、知识库、ContextBuilder、权限、审计和一致性边界。 |
| [Agent Discovery](docs/en/agent-discovery.md) | 定义 AgentDiscoveryService 如何扫描、识别、校验、缓存和注册 Agent、Adapter、MCP、Skill、Lua capability。 |
| [Hardware Hotplug](docs/en/hardware-hotplug.md) | 定义 HardwareDiscoveryService、DeviceRegistry、DriverBinding、HardwareAdapterRuntime、设备热插拔、硬件 Topic 和安全边界。 |
| [Project Configuration](docs/en/project-configuration.md) | 定义 `config/` 目录、`eva.yaml`、Agent/Adapter/Capability manifest、policy、schema 和热加载策略。 |
| [Process-Level Upgrade](docs/en/process-level-upgrade.md) | 定义 OS service manager、Supervisor、Runtime、Ingress Gate、Durable Event Log、State Store 和双活切流。 |
| [Design Risk Review](docs/en/design-risk-review.md) | 评审当前方案在 Bot 行为、事件一致性、状态归属、权限闭包、capability 语义和错误恢复上的设计风险。 |

## 当前方案定位

当前方案目标是设计一套 Rust 托管运行时、Lua 热更新 Agent、Topic EventBus、动态 Adapter、MCP 双向集成、HardwareAdapter 和进程级恢复机制组合的多 Agent 调度系统。

核心边界：

- Rust 管系统边界、权限、schema、沙箱、密钥、进程生命周期、审计、超时和恢复。
- Lua 管可热更新业务逻辑、Agent 局部状态、工具调用编排和结果转换。
- Topic EventBus 管 Agent 间协作，不承担隐式全局业务状态。
- Adapter 管外部能力接入，包括 CLI、HTTP、MCP、Skill、本地模型、内部 Agent 和硬件。
- Discovery 只做发现与归一化，不代表授权执行；执行前仍必须经过 manifest、schema 和 policy。
- 外接硬件通过 `hardware` transport 和 HardwareAdapter 接入；Lua 不直接访问设备句柄、系统设备路径或 raw IO。
- Hot reload 默认只覆盖脚本、manifest 中可热加载字段、路由和注册表 generation；权限扩大、transport、MCP command、状态 backend 等需要重建 runtime 或 blue-green。

## 当前主要缺口

当前 `docs/` 是架构方案集合，还不是最终实现规范。实现前仍需要把以下内容落成机器可校验契约：

- `AgentManifest`、`AdapterManifest`、`CapabilityManifest`、MCP policy、hardware policy 和 sandbox policy 的 JSON Schema。
- `ctx.tools` 与 `ctx.host` 的 Lua binding API。
- capability 命名注册表和冲突处理规则。
- MCP server 认证、会话隔离和 per-client rate limit。
- 写 workspace 的 Adapter、Skill、Lua capability 的审计字段与回滚建议。
- 外接硬件 manifest 的设备匹配、logical binding、generation 和 raw IO policy 需要落成可校验 schema。

方案已经覆盖目标架构，但仍需把 Bot 行为语义、状态一致性、权限合并、capability 注册、Lua binding、schema、错误恢复和验证不变量继续固化为可执行规格。详细风险见 [方案设计风险评审](docs/方案设计风险评审.md)。
