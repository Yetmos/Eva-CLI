# eva-lua-host/src

更新时间：2026-07-07

| 文件 | V1.7.1 状态 | 说明 |
| --- | --- | --- |
| `loader.rs` | 已实现 | `LuaScript::load` 和 `LuaScript::from_source`。 |
| `sandbox.rs` | 已实现 | `LuaSandboxPolicy`，拒绝危险 token。 |
| `vm.rs` | 已实现 | `LuaVmAdapter` 和 `MluaVmAdapter`；编译/执行 Lua chunk，映射语法/runtime/result 错误，只加载受限标准库，并在 V1.7.2.3 接入 `ctx.tools.call` 受控 capability 调用。 |
| `bindings.rs` | 已实现 | `LuaHost`、`LuaHostContext`、`LuaEventResult`；先走真实 VM `on_event`，必要时回退旧静态 parser，透传 V1.2 `LuaContextSnapshot`，并提供 `run_on_event_with_tools` 注入 `CapabilityHostApi`。 |
| `hot_reload.rs` | 已接 runtime marker | `LuaGeneration` marker；V0.5 `BasicRunReport` 输出 generation id 和 script count。 |
| `lib.rs` | 已实现 | re-export `LuaHost`、`LuaVmAdapter`、`MluaVmAdapter` 和既有公开类型。 |

V1.7.2.3 已经在真实 Lua VM execution boundary 上补齐只读 `ctx.request`、`ctx.trace`、`ctx.memory`、`ctx.host.log/audit` 和 `ctx.tools.call`，同时保留 V1.7.1 顶层 context 计数字段兼容旧脚本。它还不是完整 Lua runtime：资源限制、shadow load、generation swap 和 rollback 仍在后续 V1.7.3-V1.7.4。`LuaHostContext` 不暴露 memory/knowledge 服务句柄、observability sink 句柄或 raw provider/file/socket/process handle，只携带 Agent id、request/trace metadata、private/global/knowledge 计数和 audit 摘要。验证：`cargo test -p eva-lua-host`。
