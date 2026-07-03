# eva-lua-host/src

更新时间：2026-07-03

| 文件 | V0.4 状态 | 说明 |
| --- | --- | --- |
| `loader.rs` | 已实现 | `LuaScript::load` 和 `LuaScript::from_source`。 |
| `sandbox.rs` | 已实现 | `LuaSandboxPolicy`，拒绝危险 token。 |
| `bindings.rs` | 已实现 | `LuaHost`、`LuaHostContext`、`LuaEventResult`，解析受控 `on_event` 返回 table。 |
| `hot_reload.rs` | 已实现边界 | `LuaGeneration` marker。真实 hot reload 留给 V0.5。 |
| `lib.rs` | 已实现 | re-export V0.4 公开类型。 |

V0.4 不是完整 Lua VM，只是 controlled table-return contract。验证：`cargo test -p eva-lua-host`。
