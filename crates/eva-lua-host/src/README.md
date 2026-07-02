# eva-lua-host/src / Lua 宿主源码

![V0.3/V0.4 runtime module flow](../../assets/eva-runtime-module-flow.svg)

本目录承载 Lua loader、sandbox、host bindings 和热更新。当前为骨架，V0.4 先实现最小 `on_event(event, ctx)` 执行边界。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 骨架 | V0.4 |
| `loader.rs` | 加载 Lua 脚本和校验入口 | 骨架 | V0.4 |
| `sandbox.rs` | 执行沙箱策略和资源限制 | 骨架 | V0.4 |
| `bindings.rs` | 暴露 typed host API 到 Lua | 骨架 | V0.4/V1.2 |
| `hot_reload.rs` | generation swap 和 rollback 边界 | 骨架 | V0.5 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 选定 Lua 引擎接入点并封装在本 crate 内。 | 下游不感知具体引擎。 |
| 2 | 实现 loader 和 entrypoint 校验。 | 缺失 `on_event` 可诊断。 |
| 3 | 映射 `SandboxPolicy` 到 runtime 限制。 | 危险库默认禁用。 |
| 4 | 暴露 `ctx.emit`、`ctx.tools`，后续接 memory。 | Lua 只能走 typed host API。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Loader | 脚本加载和 entrypoint | 未实现 | 定义 descriptor。 |
| Sandbox | 禁用库、内存、超时 | 未实现 | 接 `SandboxPolicy`。 |
| Bindings | host API trait | 未实现 | 定义 `ctx` 能力。 |
| Hot reload | generation 切换 | 未实现 | 设计 validate/activate。 |
