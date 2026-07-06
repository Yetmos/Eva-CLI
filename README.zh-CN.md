# Eva-CLI

> 语言：[English](README.md) | 简体中文

Eva-CLI 当前已经推进到 V1.5 发布加固检查点。仓库内已有可编译 workspace、配置样例、schema、基础契约 crate、项目配置加载、V1.0 in-memory basic runtime、V1.1 外部能力诊断、V1.2 记忆/知识上下文、V1.3 硬件发现/probe/plan-first 绑定、V1.4 的 backup、snapshot、restore plan 和 upgrade check，以及 V1.5 的 release check/security/perf/migration。

当前受管理项目版本：`V1.5.0-release`（`Cargo.toml` 版本 `1.5.0`，稳定 Git tag 形式 `v1.5.0`）。版本规则见 [版本管理方案](docs/zh-CN/release/版本管理方案.md)。

官网：

- https://Eva-CLI.com
- https://www.Eva-CLI.com

官网源码维护在 [website/](website/)，文档维护在 [docs/](docs/)，Rust 源码维护在 [src/](src/) 和 [crates/](crates/)。

## 当前进度

Eva-CLI 已经完成 V0.1 到 V1.5 的阶段实现：

1. Rust workspace 和 20 个 crate 边界已创建；
2. `eva-core`、`eva-config`、`eva-policy`、`eva-observability` 已具备基础契约；
3. `eva-runtime` 已实现 V1.0 `in_memory_v1.0` basic runtime 和本地 task 诊断；
4. `eva-cli` 已实现 `version`、`doctor`、`config validate`、`inspect`、`run --example basic`、`task status/logs/cancel`、`adapter`、`mcp`、`skill`、`discovery`、`memory context`、`hardware list/probe/bind`、`backup/snapshot/restore/upgrade` 和 `release check/security/perf/migration`；
5. `eva-hardware` 已实现 V1.3 discovery candidate、DeviceRegistry lease、simulated driver binding 和 hotplug state machine；
6. `eva-backup` 和 `eva-lifecycle` 已实现 V1.4 backup artifact verification、migration preflight、release snapshot、restore plan、generation handoff、drain 和 rollback plan；
7. `eva-release` 已实现 V1.5 release readiness、security review、performance baseline、migration guide 和 compatibility policy。

完整阶段划分见 [从零到 1.0 版本路线图](docs/zh-CN/planning/从零到1.0版本路线图.md)。
V1.5.0 的 GitHub 托管发版流程见 [V1.5 GitHub 发版计划](docs/zh-CN/release/V1.5-GitHub发版计划.md)。
版本命名、递增、tag、GitHub Release 和 package 规则见 [版本管理方案](docs/zh-CN/release/版本管理方案.md)。

## eva-core 模块要实现的功能

`eva-core` 是 Eva-CLI Rust workspace 的基础契约层。它不负责启动 runtime、执行 Lua、访问网络或落盘数据，而是先把下游 crate 会共同依赖的稳定数据模型定义清楚。当前源码已经有 `event`、`topic`、`ids`、`capability`、`invoke` 和 `error` 模块占位，第一阶段要把这些占位落成可测试、可序列化、无副作用的契约类型。

`eva-core` 的具体实现范围：

- Topic 契约：实现 `Topic` 和 `TopicPattern` 的解析、格式校验和通配匹配，支持 exact、`*`、`**`，并拒绝空段、非法前缀以及位置不合法的 `**`。
- ID 契约：实现 `AgentId`、`AdapterId`、`CapabilityName`、`RequestId`、`EventId` 等 newtype，提供解析、显示和序列化能力，避免把不同 ID 当普通字符串混用。
- Event 契约：实现 `Event`、`EventTarget`、payload、时间戳、`correlation_id`、`causation_id` 等链路字段，让 EventBus、Scheduler 和 AgentRuntime 使用同一事件结构。
- Invoke 契约：实现 Agent、Capability、Adapter 调用请求与响应结构，包括调用目标、输入 payload、状态、输出和错误承载方式。
- Capability 契约：实现能力命名和 provider 选择所需的基础类型，为 `eva-capability`、`eva-adapter` 和 Agent 工具调用提供统一引用。
- Error 契约：实现 `EvaError`、`ErrorKind`、`retryable`、provider code 等结构化错误模型，作为跨 crate 的统一错误边界。

`eva-core` 明确不实现：事件持久化、订阅表、Agent mailbox、调度策略、Lua binding、Adapter transport、MCP 协议、policy 合并、runtime builder、CLI 命令、文件系统/网络/数据库/shell/硬件访问。这些职责分别归 `eva-eventbus`、`eva-scheduler`、`eva-agent`、`eva-lua-host`、`eva-adapter`、`eva-mcp`、`eva-policy`、`eva-runtime` 和 `eva-cli` 等模块。

详细设计见 [eva-core 模块设计](docs/zh-CN/architecture/eva-core模块设计.md) 和 [crates/eva-core/README.md](crates/eva-core/README.md)。

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

![Eva-CLI 总体架构](assets/eva-cli-architecture.zh-CN.svg)

这张图概括当前方案的主链路：入口与配置热加载进入 Rust 托管 Runtime，经过 Recoverable EventBus 和 Scheduler 投递到 Lua Agent；Lua 只在沙箱内处理业务逻辑，并通过 Rust Tool Layer、AdapterRegistry、MemoryService、KnowledgeService 和 HardwareAdapter 访问受控能力。

## 文档入口

默认文档入口：

- [English docs](docs/en/README.md)：默认公开入口和稳定 slug。
- [简体中文文档](docs/zh-CN/README.md)：当前详案事实主源。
- [文档维护入口](docs/README.md)

建议先按以下顺序阅读英文默认入口；需要实现级细节时，以对应中文详案为准：

1. [Architecture Overview](docs/en/architecture/architecture-overview.md)：先建立系统边界、核心模块和总体结论。
2. [Rust, Lua, and EventBus Scheduler](docs/en/architecture/rust-lua-eventbus-scheduler.md)：理解 Runtime、EventBus、Scheduler、Lua Agent 和 Topic 路由。
3. [Lua External Agent Adapter](docs/en/capabilities/lua-external-agent-adapter.md)：理解外部 Agent、CLI、HTTP、MCP、Skill 如何通过 Adapter 接入。
4. [Lua Skill, MCP, and Tool Hot Reload](docs/en/capabilities/lua-skill-mcp-tool-hot-reload.md)：理解 Tool、Lua Skill 和 MCP tool handler 如何下沉到 Lua 并热更新。
5. [Skill Implementation Plan](docs/en/capabilities/skill-implementation.md)：理解 workflow Skill、runtime worker 和 Lua Skill 如何进入受控 `workflow.*` capability。
6. [Agent Memory and Knowledge Base](docs/en/capabilities/agent-memory-knowledge-base.md)：理解 Agent 私有记忆、系统总记忆库、知识库和上下文构建边界。
7. [Agent Discovery](docs/en/capabilities/agent-discovery.md)：理解项目配置、用户环境、MCP、Skill 和 Lua capability 如何被发现与注册。
8. [Hardware Hotplug](docs/en/capabilities/hardware-hotplug.md)：理解 USB、串口、BLE、网络设备和厂商 SDK 设备如何通过 HardwareAdapter 接入并支持热插拔。
9. [Project Configuration](docs/en/operations/project-configuration.md)：理解 YAML 配置、schema、policy、manifest 和热加载边界。
10. [Process-Level Upgrade](docs/en/operations/process-level-upgrade.md)：理解 Supervisor、Runtime generation、blue-green、draining、恢复和回滚。
11. [Backup, Migration Package, and Release Snapshot](docs/en/operations/backup-migration-release-snapshot.md)：理解为什么备份、迁移包、release snapshot、restore 和 rollback 的可信执行应归 Runtime，Agent 只负责请求与解释。
12. [Design Risk Review](docs/en/planning/design-risk-review.md)：集中查看当前方案的纯设计风险、语义缺口和需要补强的设计面。
13. [Zero to 1.0 Roadmap](docs/en/planning/zero-to-one-roadmap.md)：了解从架构文档到模块划分、契约定义、最小运行闭环和 1.0 发布准备的阶段路径。
14. [Command-Line Tool Feature Design](docs/en/tooling/command-line-tool-feature-design.md)：把 Runtime 架构收束为 `eva` 命令表面，包括命令组、输出契约、安全闸口和发布优先级。

## 文档职责

| 文档 | 职责 |
| --- | --- |
| [Architecture Overview](docs/en/architecture/architecture-overview.md) | 总入口，统一系统目标、非目标、模块边界、运行链路、安全原则和待补设计。 |
| [Rust, Lua, and EventBus Scheduler](docs/en/architecture/rust-lua-eventbus-scheduler.md) | 定义 Rust Runtime、Lua Agent、EventBus、Topic、Scheduler、状态、一致性和热更新。 |
| [Lua External Agent Adapter](docs/en/capabilities/lua-external-agent-adapter.md) | 定义 AdapterRegistry、AdapterRouter、McpAdapter、SkillAdapter、HardwareAdapter、stdio/http/eventbus/hardware 等外部能力接入。 |
| [Lua Skill, MCP, and Tool Hot Reload](docs/en/capabilities/lua-skill-mcp-tool-hot-reload.md) | 定义 `lua_tool`、`lua_skill`、`lua_mcp_handler`、Lua Capability Runtime、host API、安全沙箱和 generation swap。 |
| [Skill Implementation Plan](docs/en/capabilities/skill-implementation.md) | 定义 Skill 分类、manifest、runtime gate、调用路由、安全边界、热更新和验证规则。 |
| [Agent Memory and Knowledge Base](docs/en/capabilities/agent-memory-knowledge-base.md) | 定义 Agent 私有记忆、系统总记忆库、知识库、ContextBuilder、权限、审计和一致性边界。 |
| [Agent Discovery](docs/en/capabilities/agent-discovery.md) | 定义 AgentDiscoveryService 如何扫描、识别、校验、缓存和注册 Agent、Adapter、MCP、Skill、Lua capability。 |
| [Hardware Hotplug](docs/en/capabilities/hardware-hotplug.md) | 定义 HardwareDiscoveryService、DeviceRegistry、DriverBinding、HardwareAdapterRuntime、设备热插拔、硬件 Topic 和安全边界。 |
| [Project Configuration](docs/en/operations/project-configuration.md) | 定义 `config/` 目录、`eva.yaml`、Agent/Adapter/Capability manifest、policy、schema 和热加载策略。 |
| [Process-Level Upgrade](docs/en/operations/process-level-upgrade.md) | 定义 OS service manager、Supervisor、Runtime、Ingress Gate、Durable Event Log、State Store 和双活切流。 |
| [Backup, Migration Package, and Release Snapshot](docs/en/operations/backup-migration-release-snapshot.md) | 定义备份、迁移包、release snapshot、restore、rollback、manifest 校验和 artifact audit 为什么应由 Runtime service 承担。 |
| [Design Risk Review](docs/en/planning/design-risk-review.md) | 评审当前方案在 Bot 行为、事件一致性、状态归属、权限闭包、capability 语义和错误恢复上的设计风险。 |
| [Zero to 1.0 Roadmap](docs/en/planning/zero-to-one-roadmap.md) | 定义从架构文档到模块划分、契约定义、最小可运行骨架、最小 Runtime 闭环、模块实现和 1.0 发布准备的阶段路径。 |
| [Command-Line Tool Feature Design](docs/en/tooling/command-line-tool-feature-design.md) | 定义目标 `eva` 命令组、全局参数、安全闸口、输出契约、exit code 和阶段化 CLI 实现优先级。 |

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

## V1.x 剩余缺口

V1.5 是源码发布与发布加固检查点，不是已经带安装包的完整 runtime 发行版。后续工作已经从“是否能落地”收窄为更具体的执行边界：

- stdio/http/MCP 等真实 provider 进程执行，包括认证、会话隔离、超时和限流。
- Durable EventBus、Scheduler、task、audit 和 artifact store，替代当前 in-memory 与本地诊断表面。
- 真实 Lua VM 执行、generation swap，以及稳定的 `ctx.tools` / `ctx.host` 绑定。
- `restore apply`、release pointer mutation、Supervisor 激活、blue-green Runtime 进程切换等破坏性 apply 路径。
- 签名 release artifact、跨平台安装包和 artifact provenance。
- 当高风险 apply 路径从 plan-only 诊断进入真实执行时，需要更深的机器可校验 schema 与 policy 检查。

当前文档已经区分“已实现诊断面”和“目标 apply 路径”。原始架构风险清单见 [方案设计风险评审](docs/zh-CN/planning/方案设计风险评审.md)，V1.5 源码发布保持稳定的契约见 [V1.5 兼容性策略](docs/zh-CN/release/V1.5兼容性策略.md)。
