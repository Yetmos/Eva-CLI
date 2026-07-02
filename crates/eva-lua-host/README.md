# eva-lua-host / Lua 执行宿主

更新时间：2026-07-02

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-lua-host` 负责 Lua state 加载、sandbox、host binding 和热更新 generation 边界。它只能暴露 typed host API 给脚本，不能让 Lua 直接访问文件、网络、shell、MCP、Adapter 或硬件实现。

## 当前模块功能说明

| 功能域 | 当前状态 | 目标行为 |
| --- | --- | --- |
| Loader | 骨架 | 加载 Agent Lua 脚本、校验入口函数、绑定 generation。 |
| Sandbox | 骨架 | 应用 `SandboxPolicy`，禁用危险库，限制内存和执行时间。 |
| Bindings | 骨架 | 暴露 `ctx.emit`、`ctx.tools`、`ctx.memory` 等 typed host API。 |
| Hot reload | 骨架 | 生成新 Lua generation，支持验证、切换和 rollback。 |
| `on_event` | 未实现 | V0.4 最小入口：`on_event(event, ctx)` 返回结构化结果。 |
| Schema/topic 校验 | 未实现 | Lua 输出和 emit topic 必须接受 policy gate。 |

## 模块边界

`eva-lua-host` 做：

- 加载和管理 Lua state generation。
- 应用 sandbox 策略和资源限制。
- 将 host API trait 转成 Lua 可调用绑定。
- 验证脚本入口、返回值和 emit 请求。

`eva-lua-host` 不做：

- 不直接读写 workspace、网络、shell、MCP、硬件。
- 不决定 capability provider 路由。
- 不保存 Agent durable state。
- 不绕过 `eva-policy` 扩大权限。

## 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V0.4 | 选择 Lua 引擎接入点并定义 host 内部 trait。 | 需要明确依赖策略 | crate 可编译，Lua 入口隐藏在本模块。 |
| 2 | V0.4 | 定义 `LuaScriptDescriptor`、loader、entrypoint 校验。 | `eva-config::AgentManifest` | 缺失 `on_event` 返回结构化错误。 |
| 3 | V0.4 | 实现 `SandboxPolicy` 到 Lua 限制的映射。 | `eva-policy` | 危险库默认禁用，超时和内存限制可测。 |
| 4 | V0.4 | 实现最小 `ctx.emit` 和 `ctx.tools.call` binding。 | `eva-core`、`eva-capability` | Lua 能通过 mock host API 调用 capability。 |
| 5 | V0.4 | 实现 `on_event(event, ctx)` 执行和返回值转换。 | `eva-agent` | Agent 能消费事件并拿到 Lua 输出。 |
| 6 | V0.5 | 增加 hot reload generation、预校验、切换和 rollback。 | `eva-lifecycle` 后续 | 旧 generation drain 后再释放。 |
| 7 | V1.2 | 增加 `ctx.memory`、`ctx.global_memory`、`ctx.knowledge`。 | `eva-memory` | 上下文使用受 policy 和 audit 限制。 |

## 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 模块导出 | 骨架 | re-export loader、sandbox、bindings、hot_reload。 |
| `src/loader.rs` | Lua 脚本加载 | `RESPONSIBILITY` 占位 | 定义脚本 descriptor、entrypoint 校验、加载错误。 |
| `src/sandbox.rs` | sandbox 策略执行 | `RESPONSIBILITY` 占位 | 将 `SandboxPolicy` 映射到 Lua runtime 限制。 |
| `src/bindings.rs` | host API 绑定 | `RESPONSIBILITY` 占位 | 定义 `ctx.emit`、`ctx.tools` 的最小 trait。 |
| `src/hot_reload.rs` | generation 切换和回滚 | `RESPONSIBILITY` 占位 | 定义 validate、activate、rollback 状态。 |
| `src/README.md` | 源码目录说明 | 简略 | 补充文件职责和进度。 |
| 单元测试 | loader/sandbox/bindings | 未开始 | 覆盖入口缺失、禁用库、host API mock。 |

## 验证计划

| 阶段 | 命令 | 目标 |
| --- | --- | --- |
| V0.4 | `cargo test -p eva-lua-host` | loader、sandbox、binding 测试通过。 |
| V0.4 | `cargo test -p eva-agent` | Agent 调用 Lua host mock 或真实最小 host。 |
| V0.5 | hot reload 集成测试 | 新 generation 验证失败时旧 generation 保持可用。 |

## English

`eva-lua-host` owns Lua state loading, sandboxing, host bindings, and hot reload generation boundaries. Lua can only access typed host APIs and must not directly access files, networks, shell, MCP, Adapter, or hardware implementations.
