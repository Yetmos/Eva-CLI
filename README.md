# Eva-CLI

Eva-CLI 当前处于架构方案整理阶段，仓库内主要内容是 `doc/` 下的设计文档，还不是可运行实现。

官网：

- https://Eva-CLI.com
- https://www.Eva-CLI.com

App 官网：

- https://EvaLauncher.com
- https://www.EvaLauncher.com

## 文档入口

建议先按以下顺序阅读：

1. [总体架构方案](doc/总体架构方案.md)：先建立系统边界、核心模块和总体结论。
2. [Rust 与 Lua 事件总线智能体调度架构方案](doc/Rust与Lua事件总线智能体调度架构方案.md)：理解 Runtime、EventBus、Scheduler、Lua Agent 和 Topic 路由。
3. [Lua 调用外部 Agent 动态 Adapter 架构方案](doc/Lua调用外部Agent动态Adapter架构方案.md)：理解外部 Agent、CLI、HTTP、MCP、Skill 如何通过 Adapter 接入。
4. [Lua 承载 Skill-MCP-Tool 热更新架构方案](doc/Lua承载Skill-MCP-Tool热更新架构方案.md)：理解 Tool、Lua Skill 和 MCP tool handler 如何下沉到 Lua 并热更新。
5. [Agent 扫描与发现架构方案](doc/Agent扫描与发现架构方案.md)：理解项目配置、用户环境、MCP、Skill 和 Lua capability 如何被发现与注册。
6. [外接硬件接入与热插拔架构方案](doc/外接硬件接入与热插拔架构方案.md)：理解 USB、串口、BLE、网络设备和厂商 SDK 设备如何通过 HardwareAdapter 接入并支持热插拔。
7. [项目配置方案](doc/项目配置方案.md)：理解 YAML 配置、schema、policy、manifest 和热加载边界。
8. [进程级停机升级架构方案](doc/进程级停机升级架构方案.md)：理解 Supervisor、Runtime generation、blue-green、draining、恢复和回滚。
9. [方案设计风险评审](doc/方案设计风险评审.md)：集中查看当前方案的纯设计风险、语义缺口和需要补强的设计面。

## 文档职责

| 文档 | 职责 |
| --- | --- |
| [总体架构方案](doc/总体架构方案.md) | 总入口，统一系统目标、非目标、模块边界、运行链路、安全原则和待补设计。 |
| [Rust 与 Lua 事件总线智能体调度架构方案](doc/Rust与Lua事件总线智能体调度架构方案.md) | 定义 Rust Runtime、Lua Agent、EventBus、Topic、Scheduler、状态、一致性和热更新。 |
| [Lua 调用外部 Agent 动态 Adapter 架构方案](doc/Lua调用外部Agent动态Adapter架构方案.md) | 定义 AdapterRegistry、AdapterRouter、McpAdapter、SkillAdapter、HardwareAdapter、stdio/http/eventbus/hardware 等外部能力接入。 |
| [Lua 承载 Skill-MCP-Tool 热更新架构方案](doc/Lua承载Skill-MCP-Tool热更新架构方案.md) | 定义 `lua_tool`、`lua_skill`、`lua_mcp_handler`、Lua Capability Runtime、host API、安全沙箱和 generation swap。 |
| [Agent 扫描与发现架构方案](doc/Agent扫描与发现架构方案.md) | 定义 AgentDiscoveryService 如何扫描、识别、校验、缓存和注册 Agent、Adapter、MCP、Skill、Lua capability。 |
| [外接硬件接入与热插拔架构方案](doc/外接硬件接入与热插拔架构方案.md) | 定义 HardwareDiscoveryService、DeviceRegistry、DriverBinding、HardwareAdapterRuntime、设备热插拔、硬件 Topic 和安全边界。 |
| [项目配置方案](doc/项目配置方案.md) | 定义 `config/` 目录、`eva.yaml`、Agent/Adapter/Capability manifest、policy、schema 和热加载策略。 |
| [进程级停机升级架构方案](doc/进程级停机升级架构方案.md) | 定义 OS service manager、Supervisor、Runtime、Ingress Gate、Durable Event Log、State Store 和双活切流。 |
| [方案设计风险评审](doc/方案设计风险评审.md) | 评审当前方案在 Bot 行为、事件一致性、状态归属、权限闭包、capability 语义和错误恢复上的设计风险。 |

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

当前 `doc/` 是架构方案集合，还不是最终实现规范。实现前仍需要把以下内容落成机器可校验契约：

- `AgentManifest`、`AdapterManifest`、`CapabilityManifest`、MCP policy、hardware policy 和 sandbox policy 的 JSON Schema。
- `ctx.tools` 与 `ctx.host` 的 Lua binding API。
- capability 命名注册表和冲突处理规则。
- MCP server 认证、会话隔离和 per-client rate limit。
- 写 workspace 的 Adapter、Skill、Lua capability 的审计字段与回滚建议。
- 外接硬件 manifest 的设备匹配、logical binding、generation 和 raw IO policy 需要落成可校验 schema。

方案已经覆盖目标架构，但仍需把 Bot 行为语义、状态一致性、权限合并、capability 注册、Lua binding、schema、错误恢复和验证不变量继续固化为可执行规格。详细风险见 [方案设计风险评审](doc/方案设计风险评审.md)。
