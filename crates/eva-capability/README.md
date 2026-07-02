# eva-capability / 能力注册

更新时间：2026-07-02

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-capability` 负责 Capability registry、capability router、generation swap 和 typed host API trait。它维护内部能力的可发现、可调用和可替换边界，不实现具体外部 provider、硬件 driver 或 MCP transport。

## 当前模块功能说明

| 功能域 | 当前状态 | 目标行为 |
| --- | --- | --- |
| Registry | 骨架 | 注册 capability descriptor、provider hint、schema、policy requirement。 |
| Router | 骨架 | 根据 explicit provider、capability name 和 policy 选择 provider。 |
| Generation | 骨架 | 支持 capability set 的生成、激活、回滚和只读 handle。 |
| Host API | 骨架 | 给 Agent/Lua 暴露受控 capability call trait。 |
| Builtin capability | 未实现 | V0.4 提供一个无副作用 capability 用于端到端闭环。 |
| 外部 provider | 未实现 | V1.1 通过 `eva-adapter` 和 `eva-mcp` 接入。 |

## 模块边界

`eva-capability` 做：

- 表示 capability、provider、schema、policy requirement。
- 在授权后选择可调用 provider handle。
- 给 Lua/Agent 提供 typed host API。
- 支持 generation 化的 registry 替换和回滚。

`eva-capability` 不做：

- 不执行 HTTP、stdio、MCP、hardware 等外部 transport。
- 不扫描 Discovery 来源。
- 不直接读配置文件。
- 不扩大 effective policy。

## 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V0.4 | 定义 `CapabilityDescriptor`、provider id、schema ref、policy requirement。 | `eva-core`、`eva-config` | 可从 manifest 构造 descriptor。 |
| 2 | V0.4 | 实现 registry add/remove/list/get 和 generation handle。 | 标准库 | generation 激活后旧 handle 只读。 |
| 3 | V0.4 | 实现 router：explicit provider 优先，capability name 兜底，policy allowlist gate。 | `eva-policy` | 未授权 provider 返回 PermissionDenied。 |
| 4 | V0.4 | 定义 `CapabilityHostApi` trait 和结构化 invoke 输入输出。 | `eva-core::InvokeRequest` | Lua/Agent 可通过 trait 调用。 |
| 5 | V0.4 | 实现一个 builtin no-op capability。 | 无外部依赖 | `examples/basic/` 能返回结构化结果。 |
| 6 | V1.1 | 接 AdapterRuntime provider handle。 | `eva-adapter` | 外部 provider 只经 adapter transport 执行。 |
| 7 | V1.3 | 接 HardwareAdapter provider handle。 | `eva-hardware`、`eva-adapter` | 设备调用受 policy 和 audit 限制。 |

## 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 模块导出 | 骨架 | re-export registry、router、generation、host_api。 |
| `src/registry.rs` | capability 注册和查询 | `RESPONSIBILITY` 占位 | 定义 descriptor、registry、index、errors。 |
| `src/router.rs` | provider 路由 | `RESPONSIBILITY` 占位 | 实现 explicit/name/provider hint 路由和 policy gate。 |
| `src/generation.rs` | generation swap | `RESPONSIBILITY` 占位 | 定义 generation id、activate、rollback-safe handle。 |
| `src/host_api.rs` | typed host API | `RESPONSIBILITY` 占位 | 定义 Agent/Lua 可调用 trait 和结果 envelope。 |
| `src/README.md` | 源码目录说明 | 简略 | 补充文件职责和进度。 |
| 单元测试 | registry/router/generation | 未开始 | 覆盖重复注册、未授权、generation 切换。 |

## 验证计划

| 阶段 | 命令 | 目标 |
| --- | --- | --- |
| V0.4 | `cargo test -p eva-capability` | registry、router、host API 测试通过。 |
| V0.4 | `cargo test -p eva-lua-host` | Lua host 可通过 trait 调用 capability。 |
| V1.1 | Adapter 集成测试 | 外部 provider 通过 AdapterRuntime 调用。 |

## English

`eva-capability` owns the capability registry, router, generation swap, and typed host API traits. Concrete provider transports belong to Adapter, MCP, or hardware modules.
