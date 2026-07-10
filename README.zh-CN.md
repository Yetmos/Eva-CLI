# Eva-CLI

> 语言：[English](README.md) | 简体中文

Eva-CLI 是一个基于 Rust 的 CLI runtime，用于受控多 Agent 工作流、发布加固、诊断、配置校验、请求级记忆/知识上下文组装、硬件绑定计划、备份/生命周期检查和源码发布运维。

Eva-CLI 当前处在 V1.11.5-alpha 开发线，并已完成 V1.17.5 文档/i18n 同步闭环。仓库内已有可编译 workspace、配置样例、schema、基础契约 crate、项目配置加载、V1.0 in-memory basic runtime、V1.1 外部能力诊断、V1.2 记忆/知识上下文、V1.3 硬件发现/probe/plan-first 绑定、V1.4 backup/snapshot/restore/upgrade planning、V1.5 release check/security/perf/migration、V1.6 durable runtime/storage、V1.7 受限 Lua VM 与热更新生命周期、V1.8 外部 provider/MCP/Skill runner 受控真实执行、V1.9 policy/discovery/memory/observability 基线、V1.10 硬件与高风险 apply gate、V1.11 release evidence gate、V1.11.4 CLI 命令模块拆分、V1.11.5 emit/agent/capability 命令证据、V1.12 daemon 控制/恢复、V1.13 provider supervision gate、V1.14 destructive restore mutation/rollback 与 service-manager abstraction、V1.15 hardware permission/hotplug safety gate 与 memory/knowledge maintenance/retrieval/redaction、V1.16.4 observability retention/rotation policy、V1.17.1 run command module split、V1.17.2 operator execution-state 字段、V1.17.3 高风险 text summary、V1.17.4 public JSON contract diff gate，以及 V1.17.5 同步后的 README、使用手册、发布说明、官网卡片、生成站点 HTML 和 i18n manifest metadata。

当前受管理项目版本：`V1.11.5-alpha`（`Cargo.toml` 版本 `1.11.5-alpha`，预发布 Git tag 形式 `v1.11.5-alpha`）。版本规则见 [版本管理方案](docs/zh-CN/release/版本管理方案.md)。

Canonical 官网：

- https://www.eva-cli.com/
- https://www.eva-cli.com/zh-CN/

官网源码维护在 [website/](website/)，文档维护在 [docs/](docs/)，Rust 源码维护在 [src/](src/) 和 [crates/](crates/)。

## 当前进度

Eva-CLI 已经完成 V0.1 到 V1.17.5 可执行/可验证能力与文档同步闭环：

1. Rust workspace 和 20 个 crate 边界已创建；
2. `eva-core`、`eva-config`、`eva-policy`、`eva-observability` 已具备基础契约；
3. `eva-runtime` 已实现 V1.0 `in_memory_v1.0` basic runtime 和本地 task 诊断；
4. `eva-cli` 已实现 `version`、`doctor`、`config validate`、`inspect`、`inspect durable`、`run --example basic`、`emit`、`daemon start/status/stop/shutdown/submit/cancel/drain/reload`、`agent status/drain/reload`、`capability list/probe/call`、`task status/logs/cancel`、`adapter`、`mcp`、`skill`、`discovery`、`memory context`、`hardware list/probe/bind`、`backup/snapshot/restore/upgrade` 和 `release check/security/perf/migration`；
5. `eva-hardware` 已实现 V1.3 discovery candidate、DeviceRegistry lease、simulated driver binding 和 hotplug state machine；
6. `eva-backup` 和 `eva-lifecycle` 已实现 V1.4 backup artifact verification、migration preflight、release snapshot、restore plan、generation handoff、drain 和 rollback plan；
7. `eva-release` 已实现 V1.17.4 release readiness、security review、performance baseline、migration guide、compatibility policy、durable recovery smoke gate、durable diagnostics smoke gate、Lua VM execution gate、Lua host bindings gate、Lua resource limits gate、Lua hot reload lifecycle gate、artifact/distribution/scanner/benchmark evidence gate、daemon/provider/hardware readiness gate 和 public JSON contract diff gate；
8. `eva-storage`、`eva-eventbus` 与 `eva-runtime` 已实现 V1.6 durable backend manifest、migration lock、filesystem EventLog、DurableEventBus、queryable dead-letter store、redrive 基线、durable task snapshot adapter、durable audit sink、runtime recovery scanner、runtime recovered audit evidence、durable backend diagnostics report 和 artifact metadata hardening；
9. `eva-lua-host` 已实现 V1.7.1 `mlua` VM adapter、受限标准库、真实 `on_event` 执行、稳定错误映射、旧静态 parser compatibility fallback、V1.7.2 只读 request/trace/memory、host observability 和 `ctx.tools.call` capability binding、V1.7.3 wall-clock timeout、instruction budget、cancellation token 和 memory budget，以及 V1.7.4 shadow load health gate；
10. `eva-memory` 和 `eva-runtime` 已实现 V1.15.6 durable memory/knowledge filesystem `index.lock`、TTL GC、`memory-gc.checkpoint`、`knowledge-rebuild.checkpoint`、stale checkpoint recovery 和 daemon `memory_maintenance` smoke evidence；V1.15.7/V1.15.8 新增受监督 retrieval provider invoke、line-based hex schema、脱敏入索引、source audit evidence 和 policy-driven memory redaction/audit。
11. `eva-observability` 已实现 V1.16.1 runtime/provider/task/restore JSONL best-effort audit sink wiring、V1.16.2 tracing subscriber bridge、V1.16.3 OpenTelemetry SDK exporter smoke 和 V1.16.4 retention/rotation/corrupt-record policy。
12. V1.17 已完成 `run --example basic` 拆分、operator execution-state 字段、高风险 text summary、public JSON contract diff suite，以及本次 V1.17.5 文档/i18n/release notes/网站同步闭环。

完整阶段划分见 [从零到 1.0 版本路线图](docs/zh-CN/planning/从零到1.0版本路线图.md)。
V1.5.0 的 GitHub 托管发版流程见 [V1.5 GitHub 发版计划](docs/zh-CN/release/V1.5-GitHub发版计划.md)。
版本命名、递增、tag、GitHub Release 和 package 规则见 [版本管理方案](docs/zh-CN/release/版本管理方案.md)。V1.11.5-alpha 及 V1.12-V1.17 同步后的发版内容见 [V1.11.5 Alpha 发布说明](docs/zh-CN/release/V1.11.5-alpha发布说明.md)，历史 V1.11.4-alpha 内容见 [V1.11.4 Alpha 发布说明](docs/zh-CN/release/V1.11.4-alpha发布说明.md)。

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
- [简体中文文档](docs/zh-CN/中文文档入口.md)：当前详案事实主源。
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

V1.11.5-alpha 是源码 alpha 与 CLI runtime 命令补齐检查点，已在 V1.11.4 模块拆分基础上补入 `emit`、`agent` 和 `capability` 命令 evidence。后续包含 package 支持的 release tag 会发布 GHCR 容器镜像 `ghcr.io/yetmos/eva-cli`、native archive metadata，以及 install-smoke/package dry-run evidence；旧 tag 不追溯重新发布。后续工作已经从“是否能落地”收窄为更具体的执行边界：

- stdio/http/MCP 等真实 provider 进程执行，包括认证、会话隔离、超时和限流。
- Durable Scheduler、runtime audit wiring、artifact metadata、memory 和 backup store，衔接当前 durable EventBus/task snapshot/audit sink 基线与本地诊断表面。
- 超出当前 shadow load、route gate、drain evidence 和 rollback audit 边界的常驻 daemon 热更新编排。
- `restore apply`、release pointer mutation、Supervisor 激活、blue-green Runtime 进程切换等破坏性 apply 路径。
- 生产签名凭据、Homebrew/Winget/Apt 仓库发布、外部 scanner 接入和更强 artifact provenance。
- 当高风险 apply 路径从 plan-only 诊断进入真实执行时，需要更深的机器可校验 schema 与 policy 检查。

当前文档已经区分“已实现诊断面”和“目标 apply 路径”。原始架构风险清单见 [方案设计风险评审](docs/zh-CN/planning/方案设计风险评审.md)，V1.5 源码发布保持稳定的契约见 [V1.5 兼容性策略](docs/zh-CN/release/V1.5兼容性策略.md)。
