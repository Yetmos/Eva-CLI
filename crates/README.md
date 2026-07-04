# Crates / Rust 子模块

更新时间：2026-07-04

![Eva module implementation roadmap](assets/eva-module-implementation-roadmap.svg)

## V1.5 Workspace Status

V1.5 keeps the V1.1 external capability checkpoint, V1.2 request-scoped memory/knowledge context, V1.3 controlled hardware boundary, and V1.4 backup/lifecycle planning, then adds release hardening across `eva-release` and `eva-cli`:

- `eva-adapter`: authorized handles, registry, router, probe, and controlled MCP/Skill invocation envelopes.
- `eva-mcp`: allowlist policy helper, in-memory client/probe, tool mapping registry, and minimal side-effect-free server surface descriptor.
- `eva-discovery`: project manifest discovery candidates, cache, health projection, and the invariant that discovery never grants executable handles.
- `eva-memory`: private/global memory records, knowledge indexing, budgeted `ContextBuilder`, and `LuaContextSnapshot`.
- `eva-lua-host`: controlled context snapshot on `LuaHostContext` and `LuaEventResult`.
- `eva-hardware`: hardware discovery candidates, trusted identities, `DeviceRegistry` claim/release, `HardwareDriver` binding, simulated driver, and hotplug state machine.
- `eva-backup`: backup artifact plans, manifest verification, migration preflight, release snapshot, and plan-first restore.
- `eva-lifecycle`: generation handoff, drain plan, rollback plan, and in-memory supervisor readiness checks.
- `eva-release`: cross-platform readiness gates, security review findings, performance budgets, migration guide, compatibility policy, and release readiness aggregation.
- `eva-cli`: `adapter list/probe`, `mcp list/probe`, `skill list/run`, `discovery scan`, `memory context`, `hardware list/probe/bind`, `backup create`, `snapshot create`, `restore plan`, `upgrade check`, and `release check/security/perf/migration` commands with text/JSON envelopes.

V1.5 proves that the implemented 1.x source surface can be checked through executable release gates without destructive mutation, external provider startup, or real process management.

本目录承载 Eva-CLI Rust workspace 的模块边界。每个 crate 对应一个稳定职责域；公共契约先在基础 crate 稳定，副作用通过 `eva-runtime` 单向组合进入。

## 总体规则

| 规则 | 说明 |
| --- | --- |
| 契约先行 | `eva-core`、`eva-config`、`eva-policy`、`eva-observability` 先稳定公共类型、配置输入、权限和观测字段。 |
| Runtime 单向组合 | `eva-runtime` 是唯一组合根，下层 crate 不反向依赖 runtime。 |
| CLI 不持有状态 | `eva-cli` 只解析命令、输出报告并调用 runtime。 |
| 外部能力受控 | Adapter、MCP、Discovery、Hardware、Backup、Lifecycle 必须经过 manifest、policy、audit gate。 |

## 模块索引

| 模块 | 职责 | 当前状态 | README |
| --- | --- | --- | --- |
| `eva-core` | 事件、Topic、Invoke、ID、错误等基础契约 | 已完成 V0.1/V0.2 | [README](eva-core/README.md) |
| `eva-config` | `eva.yaml`、manifest、routes、policy document 加载 | 已完成 V0.2 | [README](eva-config/README.md) |
| `eva-policy` | 权限集合、sandbox policy、effective policy | 已完成 V0.2 | [README](eva-policy/README.md) |
| `eva-observability` | trace、audit、metrics 契约 | 已完成 V0.2 | [README](eva-observability/README.md) |
| `eva-cli` | CLI parser、formatter、exit code、运行入口、version/task/external/memory/hardware/backup/lifecycle/release 命令 | 已完成 V1.5 release hardening command surface | [README](eva-cli/README.md) |
| `eva-runtime` | 组合根、builder、service summary、basic loop、task report | 已完成 V1.0 core runtime mode | [README](eva-runtime/README.md) |
| `eva-storage` | StateStore、EventLog、ArtifactStore | 已完成 V0.4 in-memory | [README](eva-storage/README.md) |
| `eva-eventbus` | publish、ack/fail、dead letter、replay | 已完成 V0.5 replay diagnostics | [README](eva-eventbus/README.md) |
| `eva-scheduler` | Topic 匹配、订阅表、mailbox 投递 | 已完成 V0.4 | [README](eva-scheduler/README.md) |
| `eva-agent` | Agent 生命周期、队列、事件处理、timeout/cancel/retry 控制 | 已完成 V0.5 run control | [README](eva-agent/README.md) |
| `eva-lua-host` | Lua loader、sandbox gate、受控 `on_event` contract、generation marker | 已完成 V0.5 generation marker | [README](eva-lua-host/README.md) |
| `eva-capability` | Capability registry、router、host API | 已完成 V0.4 builtins | [README](eva-capability/README.md) |
| `eva-adapter` | Adapter manifest、registry、router、transport runtime | 已完成 V1.3 hardware transport boundary | [README](eva-adapter/README.md) |
| `eva-mcp` | MCP client/server、tool mapping、schema | 已完成 V1.1 side-effect-free MCP surface | [README](eva-mcp/README.md) |
| `eva-discovery` | 受信发现源、归一化、健康探测、缓存 | 已完成 V1.1 discovery candidate surface | [README](eva-discovery/README.md) |
| `eva-memory` | 私有记忆、全局记忆、知识库、上下文构建 | 已完成 V1.2 context layer | [README](eva-memory/README.md) |
| `eva-hardware` | 设备发现、driver binding、hotplug | 已完成 V1.3 controlled hardware boundary | [README](eva-hardware/README.md) |
| `eva-backup` | 备份、迁移包、release snapshot、校验 | 已完成 V1.4 backup/snapshot planning boundary | [README](eva-backup/README.md) |
| `eva-lifecycle` | supervisor、generation、drain、rollback | 已完成 V1.4 lifecycle planning boundary | [README](eva-lifecycle/README.md) |
| `eva-release` | release readiness、安全评审、性能预算、迁移指南、兼容策略 | 已完成 V1.5 release hardening boundary | [README](eva-release/README.md) |

## 项目级实施进度

| 版本 | 模块 | 关键能力 | 当前进度 | 完成判据 |
| --- | --- | --- | --- | --- |
| V0.1 | workspace、`eva-core` | crate 划分、基础契约、文档图谱 | 已完成 | `cargo test --workspace` 通过 |
| V0.2 | `eva-config`、`eva-policy`、`eva-observability` | 配置加载、权限收缩、观测字段 | 已完成 | 模块测试和 workspace 测试通过 |
| V0.3 | `eva-cli`、`eva-runtime`、`eva-config` | `doctor`、`config validate`、`inspect`、no-op runtime builder | 已完成 | CLI 结构化诊断和 runtime summary 可读 |
| V0.4 | storage/eventbus/scheduler/agent/lua-host/capability/runtime/cli | 最小事件运行闭环 | 已完成 | `cargo run -- run --example basic --output json` 成功 |
| V0.5 | `eva-agent`、`eva-lua-host`、`eva-eventbus`、`eva-runtime`、`eva-cli` | 任务状态、日志、取消、超时、重试、dead-letter replay、generation marker | 已完成 | `task status/logs/cancel` 可读本地 task report；timeout/cancel/replay 可验证 |
| V1.0 | `eva-cli`、`eva-runtime`、docs、CI | version 命令、`in_memory_v1.0`、quickstart、release notes、已知限制、CI/release gates | 已完成 | 新用户可从源码构建并跑通 V1.0 quickstart |
| V1.1 | `eva-adapter`、`eva-mcp`、`eva-discovery`、`eva-cli` | 外部能力发现、probe、受控 envelope 调用 | 已完成 | `adapter list/probe`、`mcp list/probe`、`skill list/run`、`discovery scan` 可验证 |
| V1.2 | `eva-memory`、`eva-lua-host`、`eva-cli` | memory、knowledge、context builder、Lua context snapshot、`memory context` | 已完成 | 上下文组装有权限、预算和审计；Lua 只接收受控快照 |
| V1.3 | `eva-hardware`、`eva-adapter`、`eva-cli` | 设备发现、绑定、hotplug、hardware transport、hardware list/probe/bind | 已完成 | Lua 不能 raw I/O；`scale-main` 默认 blocked plan-first |
| V1.4 | `eva-backup`、`eva-lifecycle`、`eva-cli` | 备份、迁移、snapshot、generation rollback、backup/snapshot/restore/upgrade commands | 已完成 | 高风险操作先 plan 后 apply；V1.4 不执行 destructive restore |
| V1.5 | `eva-release`、`eva-cli`、docs、CI | 安全、性能、发布验收、迁移指南、兼容策略、release commands | 已完成 | `release check/security/perf/migration` 全部通过 |

## 共享插图

| 图 | 用途 | 文件 |
| --- | --- | --- |
| 模块实施路线图 | 所有模块 README 的版本基线 | [eva-module-implementation-roadmap.svg](assets/eva-module-implementation-roadmap.svg) |
| 运行闭环模块流 | V0.3-V0.5 相关模块 | [eva-runtime-module-flow.svg](assets/eva-runtime-module-flow.svg) |
| 扩展生态模块流 | V1.x 扩展模块 | [eva-extension-module-flow.svg](assets/eva-extension-module-flow.svg) |

## 维护要求

实现模块功能时，同步更新对应 crate README 和 `src/README.md`。公共契约变更先更新基础 crate，再更新下游模块，最后运行 workspace 级验证。
