# Crates / Rust 子模块

更新时间：2026-07-03

![Eva module implementation roadmap](assets/eva-module-implementation-roadmap.svg)

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
| `eva-cli` | CLI parser、formatter、exit code、运行入口、task 命令 | 已完成 V0.5 task diagnostics | [README](eva-cli/README.md) |
| `eva-runtime` | 组合根、builder、service summary、basic loop、task report | 已完成 V0.5 task diagnostics | [README](eva-runtime/README.md) |
| `eva-storage` | StateStore、EventLog、ArtifactStore | 已完成 V0.4 in-memory | [README](eva-storage/README.md) |
| `eva-eventbus` | publish、ack/fail、dead letter、replay | 已完成 V0.5 replay diagnostics | [README](eva-eventbus/README.md) |
| `eva-scheduler` | Topic 匹配、订阅表、mailbox 投递 | 已完成 V0.4 | [README](eva-scheduler/README.md) |
| `eva-agent` | Agent 生命周期、队列、事件处理、timeout/cancel/retry 控制 | 已完成 V0.5 run control | [README](eva-agent/README.md) |
| `eva-lua-host` | Lua loader、sandbox gate、受控 `on_event` contract、generation marker | 已完成 V0.5 generation marker | [README](eva-lua-host/README.md) |
| `eva-capability` | Capability registry、router、host API | 已完成 V0.4 builtins | [README](eva-capability/README.md) |
| `eva-adapter` | Adapter manifest、registry、router、transport runtime | 骨架 | [README](eva-adapter/README.md) |
| `eva-mcp` | MCP client/server、tool mapping、schema | 骨架 | [README](eva-mcp/README.md) |
| `eva-discovery` | 受信发现源、归一化、健康探测、缓存 | 骨架 | [README](eva-discovery/README.md) |
| `eva-memory` | 私有记忆、全局记忆、知识库、上下文构建 | 骨架 | [README](eva-memory/README.md) |
| `eva-hardware` | 设备发现、driver binding、hotplug | 骨架 | [README](eva-hardware/README.md) |
| `eva-backup` | 备份、迁移包、release snapshot、校验 | 骨架 | [README](eva-backup/README.md) |
| `eva-lifecycle` | supervisor、generation、drain、rollback | 骨架 | [README](eva-lifecycle/README.md) |

## 项目级实施进度

| 版本 | 模块 | 关键能力 | 当前进度 | 完成判据 |
| --- | --- | --- | --- | --- |
| V0.1 | workspace、`eva-core` | crate 划分、基础契约、文档图谱 | 已完成 | `cargo test --workspace` 通过 |
| V0.2 | `eva-config`、`eva-policy`、`eva-observability` | 配置加载、权限收缩、观测字段 | 已完成 | 模块测试和 workspace 测试通过 |
| V0.3 | `eva-cli`、`eva-runtime`、`eva-config` | `doctor`、`config validate`、`inspect`、no-op runtime builder | 已完成 | CLI 结构化诊断和 runtime summary 可读 |
| V0.4 | storage/eventbus/scheduler/agent/lua-host/capability/runtime/cli | 最小事件运行闭环 | 已完成 | `cargo run -- run --example basic --output json` 成功 |
| V0.5 | `eva-agent`、`eva-lua-host`、`eva-eventbus`、`eva-runtime`、`eva-cli` | 任务状态、日志、取消、超时、重试、dead-letter replay、generation marker | 已完成 | `task status/logs/cancel` 可读本地 task report；timeout/cancel/replay 可验证 |
| V1.1 | `eva-adapter`、`eva-mcp`、`eva-discovery` | 外部能力发现、probe、受控调用 | 待实现 | 外部能力只经 policy gate 执行 |
| V1.2 | `eva-memory`、`eva-lua-host` | memory、knowledge、context builder | 待实现 | 上下文组装有权限和审计 |
| V1.3 | `eva-hardware`、`eva-adapter` | 设备发现、绑定、hotplug、hardware transport | 待实现 | Lua 不能 raw I/O |
| V1.4 | `eva-backup`、`eva-lifecycle` | 备份、迁移、snapshot、generation rollback | 待实现 | 高风险操作先 plan 后 apply |
| V1.5 | 全模块 | 安全、性能、发布验收 | 待实现 | release checklist 全部通过 |

## 共享插图

| 图 | 用途 | 文件 |
| --- | --- | --- |
| 模块实施路线图 | 所有模块 README 的版本基线 | [eva-module-implementation-roadmap.svg](assets/eva-module-implementation-roadmap.svg) |
| 运行闭环模块流 | V0.3-V0.5 相关模块 | [eva-runtime-module-flow.svg](assets/eva-runtime-module-flow.svg) |
| 扩展生态模块流 | V1.x 扩展模块 | [eva-extension-module-flow.svg](assets/eva-extension-module-flow.svg) |

## 维护要求

实现模块功能时，同步更新对应 crate README 和 `src/README.md`。公共契约变更先更新基础 crate，再更新下游模块，最后运行 workspace 级验证。
