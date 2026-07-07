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
| 上下文 | `LuaHostContext` | 携带当前 Agent id 和 `LuaContextSnapshot`，快照只包含 private/global/knowledge 计数与 audit 摘要。 |
| 结果 | `LuaEventResult` | 返回 agent id、status、topic、note、capability、capability_input 和受控 context snapshot。 |
| generation | `LuaGeneration` | 保存 generation id 和脚本数量；V0.5 runtime report 会输出该 marker。 |

## V1.7.1 VM Execution Boundary

V1.7.1 开始，`LuaHost::run_on_event()` 会先通过 `MluaVmAdapter` 执行真实 Lua：

- 脚本可以 `return root`，也可以定义 global `on_event` 或 `root.on_event`。
- host 注入只读 event table：`event.event_id`、`event.topic`、`event.payload`。
- host 注入受控 context table：`ctx.agent_id`、`private_memory_count`、`global_memory_count`、`knowledge_count` 和 `audit`。
- Lua 返回 table 会转换为既有 `LuaEventResult`，继续复用 `status`、`agent_id`、`topic`、`note`、`capability` 和 `capability_input` 字段。
- 语法错误映射为 `lua_syntax_error`，runtime error 映射为 `lua_runtime_error`，错误消息不包含宿主文件路径。

V1.7.1 不实现 `ctx.tools`、`ctx.host.audit/log`、timeout/instruction budget、memory limit、shadow load、generation swap 或 rollback。这些能力留给 V1.7.2-V1.7.4。

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
构造的 `LuaContextSnapshot`。该快照只暴露：

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

已覆盖：危险 token 拒绝、真实 Lua `on_event` 执行、受限标准库、语法错误映射、runtime error 映射、compatibility fallback、capability 请求解析、受控上下文快照透传。

## 后续计划

| 版本 | 计划 |
| --- | --- |
| V1.0 | 已在 quickstart、known limitations 和 release notes 中明确 controlled contract 限制。 |
| V1.7.1 | 已接入真实 Lua VM execution boundary 和 compatibility fallback。 |
| V1.7.2+ | 接入 host API、timeout/memory limit、shadow load、generation swap 和 rollback。 |
| V1.2 | 已接入 `LuaContextSnapshot`，作为 `ctx.memory`、`ctx.global_memory`、`ctx.knowledge` 受控 API 的最小边界。 |
