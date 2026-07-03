# eva-lua-host / Lua Host 边界

更新时间：2026-07-03

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-lua-host` 负责 Lua 脚本加载、最小 sandbox gate、受控 `on_event` 契约、generation 标记和 V1.2 受控上下文快照。V1.2 仍不引入真实 Lua VM；Lua 只能接收 `LuaContextSnapshot` 摘要，不能直接持有 `MemoryService`、`KnowledgeService` 或其他 Agent 的私有记忆句柄。

## 当前实现

| 能力 | 类型/文件 | 当前行为 |
| --- | --- | --- |
| 脚本加载 | `LuaScript` | 从路径读取脚本，或从测试 source 构造脚本对象。 |
| sandbox gate | `LuaSandboxPolicy` | 禁止 `os.execute`、`io.popen`、`require`、`dofile`、`loadfile` 等危险 token。 |
| Host facade | `LuaHost` | 验证 sandbox，检查 `on_event`，解析返回 table 中的静态字符串字段。 |
| 上下文 | `LuaHostContext` | 携带当前 Agent id 和 `LuaContextSnapshot`，快照只包含 private/global/knowledge 计数与 audit 摘要。 |
| 结果 | `LuaEventResult` | 返回 agent id、status、topic、note、capability、capability_input 和受控 context snapshot。 |
| generation | `LuaGeneration` | 保存 generation id 和脚本数量；V0.5 runtime report 会输出该 marker。 |

## V0.5 合约限制

V0.5 不执行任意 Lua 语句，也不计算 Lua 表达式。它只解析同一行的静态字符串字段，例如：

```lua
status = "accepted"
capability = "config.lint"
capability_input = "examples/basic/config"
```

如果脚本写出 `topic = event and event.topic or nil`，host 会回退使用输入事件 topic。后续接入真实 Lua VM 时，必须继续保持 sandbox、host API 和 policy gate 的边界。

## Hot Reload 边界

V0.5 的 `LuaGeneration` 是 marker，不是 VM swap：

- V1.0 runtime 使用 `basic-v1.0` generation id 生成报告，V0.5 builder 仍保留兼容入口。
- `script_count` 记录当前 basic runtime 加载的启用 Agent 脚本数量。
- 不迁移 Lua VM 内部状态，不执行 shadow load，不实现 rollback。

## 公开入口

```rust
use eva_lua_host::{LuaGeneration, LuaHost, LuaHostContext, LuaScript};
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

已覆盖：危险 token 拒绝、`on_event` 静态字段解析、capability 请求解析、受控上下文快照透传。

## 后续计划

| 版本 | 计划 |
| --- | --- |
| V1.0 | 已在 quickstart、known limitations 和 release notes 中明确 controlled contract 限制。 |
| V1.x | 接入真实 Lua VM、timeout/memory limit、shadow load、generation swap 和 rollback。 |
| V1.2 | 已接入 `LuaContextSnapshot`，作为 `ctx.memory`、`ctx.global_memory`、`ctx.knowledge` 受控 API 的最小边界。 |
