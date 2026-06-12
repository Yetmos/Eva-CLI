# EvaLauncher-CLI 架构文档索引

更新日期：2026-06-12

## 1. 阅读顺序

建议按以下顺序阅读：

1. `总体架构方案.md`：先建立系统边界、核心模块和总体结论。
2. `Rust与Lua事件总线智能体调度架构方案.md`：理解 Runtime、EventBus、Scheduler、Lua Agent 和 Topic 路由。
3. `Lua调用外部Agent动态Adapter架构方案.md`：理解外部 Agent、CLI、HTTP、MCP、Skill 如何通过 Adapter 接入。
4. `Lua承载Skill-MCP-Tool热更新架构方案.md`：理解 Tool、Lua Skill 和 MCP tool handler 如何下沉到 Lua 并热更新。
5. `Agent扫描与发现架构方案.md`：理解项目配置、用户环境、MCP、Skill 和 Lua capability 如何被发现与注册。
6. `项目配置方案.md`：理解 YAML 配置、schema、policy、manifest 和热加载边界。
7. `进程级停机升级架构方案.md`：理解 Supervisor、Runtime generation、blue-green、draining、恢复和回滚。

## 2. 文档职责

| 文档 | 职责 |
| --- | --- |
| `总体架构方案.md` | 总入口，统一系统目标、非目标、模块边界、运行链路、安全原则和待补设计。 |
| `Rust与Lua事件总线智能体调度架构方案.md` | 定义 Rust Runtime、Lua Agent、EventBus、Topic、Scheduler、状态、一致性和热更新。 |
| `Lua调用外部Agent动态Adapter架构方案.md` | 定义 AdapterRegistry、AdapterRouter、McpAdapter、SkillAdapter、stdio/http/eventbus 等外部能力接入。 |
| `Lua承载Skill-MCP-Tool热更新架构方案.md` | 定义 `lua_tool`、`lua_skill`、`lua_mcp_handler`、Lua Capability Runtime、host API、安全沙箱和 generation swap。 |
| `Agent扫描与发现架构方案.md` | 定义 AgentDiscoveryService 如何扫描、识别、校验、缓存和注册 Agent、Adapter、MCP、Skill、Lua capability。 |
| `项目配置方案.md` | 定义 `config/` 目录、`eva.yaml`、Agent/Adapter/Capability manifest、policy、schema 和热加载策略。 |
| `进程级停机升级架构方案.md` | 定义 OS service manager、Supervisor、Runtime、Ingress Gate、Durable Event Log、State Store 和双活切流。 |

## 3. 核心边界

- Rust 是能力边界：负责权限、schema、沙箱、密钥注入、MCP transport、进程生命周期、审计、超时、并发和回滚。
- Lua 是可热更新业务实现：负责 Agent 业务逻辑、Lua Tool、Lua Skill、MCP tool handler、参数映射、结果转换和轻量编排。
- Discovery 只做发现与归一化，不代表授权执行；执行前仍必须经过 manifest、schema 和 policy。
- Adapter 是外部能力统一入口；MCP、Skill、CLI、HTTP、本地模型和内部 Agent 都通过 capability 暴露。
- Hot reload 默认只覆盖脚本、manifest 中可热加载字段、路由和注册表 generation；权限扩大、transport、MCP command、状态 backend 等需要重建 runtime 或 blue-green。

## 4. 当前文档状态

当前 `doc/` 是架构方案集合，还不是最终实现规范。实现前仍需要把以下内容落成机器可校验契约：

- `AgentManifest`、`AdapterManifest`、`CapabilityManifest`、MCP policy 和 sandbox policy 的 JSON Schema。
- `ctx.tools` 与 `ctx.host` 的 Lua binding API。
- capability 命名注册表和冲突处理规则。
- MCP server 认证、会话隔离和 per-client rate limit。
- 写 workspace 的 Adapter、Skill、Lua capability 的审计字段与回滚建议。
