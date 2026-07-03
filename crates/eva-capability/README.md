# eva-capability / Capability 注册与路由

更新时间：2026-07-03

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-capability` 负责 capability descriptor、registry、router、generation marker 和给 Lua/Agent 使用的 typed host API。V0.4 已实现内存注册表和两个无外部副作用 builtin：`config.lint`、`runtime.echo`。

## V0.4 当前实现

| 能力 | 类型/文件 | 当前行为 |
| --- | --- | --- |
| Descriptor | `CapabilityDescriptor` | 可从 manifest 构造，也可构造 builtin descriptor。 |
| Registry | `CapabilityRegistry` | 支持 register、get、list；重复 capability name 返回 `Conflict`。 |
| Builtins | `with_v04_builtins` | 注册 `config.lint` 和 `runtime.echo`。 |
| Router | `CapabilityRouter` | 仅接受 `InvokeTarget::Capability`，查 registry 后执行 builtin provider。 |
| Host API | `CapabilityHostApi` | 定义 `invoke(InvokeRequest) -> InvokeResponse`。 |
| Generation | `CapabilityGeneration` | 保存 generation id 和 capability count。 |

## Builtin 行为

| Capability | 输出 |
| --- | --- |
| `config.lint` | `{"valid":true,"findings":[],"input":"..."}` |
| `runtime.echo` | `{"echo":"..."}` |

这些 builtin 用于 V0.4 闭环证明，不调用外部 provider，不读取文件，不执行 shell。

## 模块边界

`eva-capability` 不实现 HTTP、stdio、MCP、hardware transport。外部 provider 运行时属于 `eva-adapter`、`eva-mcp`、`eva-hardware` 等后续模块。Lua 和 Agent 只能通过 `CapabilityHostApi` 间接调用 capability。

## 公开入口

```rust
use eva_capability::{CapabilityHostApi, CapabilityRouter};
```

## 验证

```powershell
cargo test -p eva-capability
```

V0.4 已覆盖：builtin registry、`config.lint` completed response。

## 后续计划

| 版本 | 计划 |
| --- | --- |
| V0.5 | 增加更完整的 provider selection、permission gate、generation handle。 |
| V1.1 | 接入 AdapterRuntime/MCP provider。 |
| V1.3 | 接入 HardwareAdapter provider。 |
