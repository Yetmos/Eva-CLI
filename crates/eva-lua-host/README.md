# eva-lua-host / Lua Host 边界

更新时间：2026-07-07

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-lua-host` 负责 Lua 脚本加载、最小 sandbox gate、受控 `on_event` 契约、V1.7.1 real VM execution boundary、generation 标记和 V1.2 受控上下文快照。Lua 只能接收 `LuaContextSnapshot` 摘要，不能直接持有 `MemoryService`、`KnowledgeService` 或其他 Agent 的私有记忆句柄。

## 当前实现

| 能力 | 类型/文件 | 当前行为 |
| --- | --- | --- |
| 脚本加载 | `LuaScript` | 从路径读取脚本，或从测试 source 构造脚本对象。 |
| sandbox gate | `LuaSandboxPolicy` | 禁止 `os.execute`、`io.popen`、`require`、`dofile`、`loadfile` 等危险 token。 |
| VM adapter | `LuaVmAdapter` / `MluaVmAdapter` | 使用 vendored Lua 5.4 编译并执行脚本 chunk；只加载 table/string/utf8/math 标准库。 |
| Host facade | `LuaHost` | 验证 sandbox，通过 VM adapter 执行 `on_event`，并在旧静态字段 contract 上保留 compatibility fallback。 |
| 上下文 | `LuaHostContext` | 携带当前 Agent id 和 `LuaContextSnapshot`；V1.7.2.1 在 Lua 中注入只读 `ctx.request`、`ctx.trace` 和 `ctx.memory` 表。 |
| Tool binding | `ctx.tools.call` | V1.7.2.3 通过 `CapabilityHostApi` 暴露受控 capability 调用，只接受 capability ref 和 JSON-compatible Lua value。 |
| 资源限制 | `LuaExecutionLimits` | V1.7.3 接入 wall-clock timeout、instruction budget、cancellation token 和 memory budget，超限时映射为稳定 provider code。 |
| 结果 | `LuaEventResult` | 返回 agent id、status、topic、note、capability、capability_input、受控 context snapshot 和 Lua host observability。 |
| generation | `LuaGeneration` | 保存 generation id 和脚本数量；V0.5 runtime report 会输出该 marker。 |

## V1.7.1 VM Execution Boundary

V1.7.1 开始，`LuaHost::run_on_event()` 会先通过 `MluaVmAdapter` 执行真实 Lua：

- 脚本可以 `return root`，也可以定义 global `on_event` 或 `root.on_event`。
- host 注入只读 event table：`event.event_id`、`event.topic`、`event.payload`。
- host 注入受控 context table：`ctx.agent_id`、`ctx.request`、`ctx.trace`、`ctx.memory`、`ctx.host`，并保留 `private_memory_count`、`global_memory_count`、`knowledge_count` 和 `audit` 顶层兼容字段。
- `ctx.host.log(level, message)` 与 `ctx.host.audit(message)` 只生成 `LuaHostObservation`，由 runtime 写入 `AuditSink`、task log 和 CLI JSON，不暴露 sink 句柄。
- `ctx.tools.call(capability, value)` 通过调用方注入的 `CapabilityHostApi` 执行受控 capability，Lua 只能看到结构化 response table，不会拿到 raw provider/file/socket/process handle。
- Lua 返回 table 会转换为既有 `LuaEventResult`，继续复用 `status`、`agent_id`、`topic`、`note`、`capability` 和 `capability_input` 字段。
- 语法错误映射为 `lua_syntax_error`，runtime error 映射为 `lua_runtime_error`，错误消息不包含宿主文件路径。
- `LuaExecutionLimits` 可以为受控 VM 配置 wall-clock timeout、instruction budget、cancellation token 和 memory budget；超时、指令预算耗尽、取消和内存超限分别映射为稳定错误 evidence。

V1.7.3.4 已实现只读 request/trace/memory context 注入、Lua host log/audit 观测事件、`ctx.tools.call` capability binding，并移除 Lua `rawset` 全局入口来避免绕过只读快照；同时补齐 timeout、instruction budget、cancellation token 和 memory limit。shadow load、generation swap 或 rollback 仍留给后续 V1.7.4 节点。

## Compatibility Fallback

历史 V0.5/V1.2 只解析同一行的静态字符串字段，例如：

```lua
status = "accepted"
capability = "config.lint"
capability_input = "examples/basic/config"
```

如果旧脚本不是有效 Lua chunk，但包含 `on_event` 和这些静态字段，V1.7.1 会在 VM load 失败后走 compatibility fallback。有效 Lua 脚本默认走真实 VM。

## Hot Reload 边界

V0.5 的 `LuaGeneration` 是 marker，不是 VM swap：

- V1.0 runtime 使用 `basic-v1.0` generation id 生成报告，V0.5 builder 仍保留兼容入口。
- `script_count` 记录当前 basic runtime 加载的启用 Agent 脚本数量。
- 不迁移 Lua VM 内部状态，不执行 shadow load，不实现 rollback。

## 公开入口

```rust
use eva_lua_host::{LuaGeneration, LuaHost, LuaHostContext, LuaScript, LuaVmAdapter};
```

## V1.2 Context Boundary

`LuaHostContext::new(agent_id)` 会创建空上下文快照；调用方也可以使用
`LuaHostContext::with_context(snapshot)` 注入由 `eva-memory::ContextBuilder`
构造的 `LuaContextSnapshot`。Lua 中的 `ctx.memory` 只暴露：

- `private_memory_count`
- `global_memory_count`
- `knowledge_count`
- `audit`

真实 memory record、knowledge item 和底层服务句柄都留在 Rust 侧。后续接入真实
Lua VM 时，必须继续保持这个边界，避免 Lua 通过 host API 绕过 Agent 私有记忆隔离。

## 验证

```powershell
cargo test -p eva-lua-host
```

已覆盖：危险 token 拒绝、真实 Lua `on_event` 执行、受限标准库、语法错误映射、runtime error 映射、compatibility fallback、capability 请求解析、受控上下文快照透传、`ctx.host` observability、`ctx.tools.call` 正常调用、未知/disabled capability 拒绝、raw handle 不暴露、无限循环 timeout、instruction budget、cancellation token 和 memory budget 超限。

## 后续计划

| 版本 | 计划 |
| --- | --- |
| V1.0 | 已在 quickstart、known limitations 和 release notes 中明确 controlled contract 限制。 |
| V1.7.1 | 已接入真实 Lua VM execution boundary 和 compatibility fallback。 |
| V1.7.2.1 | 已接入只读 `ctx.request`、`ctx.trace` 和 `ctx.memory` 表。 |
| V1.7.2.2 | 已接入 `ctx.host.log/audit`，由 runtime 写入 observability evidence。 |
| V1.7.2.3 | 已接入 `ctx.tools.call`，通过 `CapabilityHostApi` 执行受控 capability 调用。 |
| V1.7.3.1 | 已接入 Lua wall-clock timeout hook。 |
| V1.7.3.2 | 已接入 Lua instruction budget。 |
| V1.7.3.3 | 已接入 Lua cancellation token。 |
| V1.7.3.4 | 已接入 Lua memory budget。 |
| V1.7.4+ | 接入 shadow load、generation swap 和 rollback。 |
| V1.2 | 已接入 `LuaContextSnapshot`，作为 `ctx.memory`、`ctx.global_memory`、`ctx.knowledge` 受控 API 的最小边界。 |
