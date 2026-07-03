# eva-lua-host/src

更新时间：2026-07-03

| 文件 | V0.5 状态 | 说明 |
| --- | --- | --- |
| `loader.rs` | 已实现 | `LuaScript::load` 和 `LuaScript::from_source`。 |
| `sandbox.rs` | 已实现 | `LuaSandboxPolicy`，拒绝危险 token。 |
| `bindings.rs` | 已实现 | `LuaHost`、`LuaHostContext`、`LuaEventResult`，解析受控 `on_event` 返回 table，并透传 V1.2 `LuaContextSnapshot`。 |
| `hot_reload.rs` | 已接 runtime marker | `LuaGeneration` marker；V0.5 `BasicRunReport` 输出 generation id 和 script count。 |
| `lib.rs` | 已实现 | re-export V0.5 公开类型。 |

V1.2 仍不是完整 Lua VM，只是在 controlled table-return contract + generation marker 基础上增加受控上下文快照。`LuaHostContext` 不暴露 memory/knowledge 服务句柄，只携带 Agent id、private/global/knowledge 计数和 audit 摘要。验证：`cargo test -p eva-lua-host`。
