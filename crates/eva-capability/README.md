# eva-capability / Capability 注册与路由

更新时间：2026-07-10

![V0.3/V0.4 runtime module flow](../assets/eva-runtime-module-flow.svg)

`eva-capability` 负责 capability descriptor、registry、router、provider selection plan、permission gate、generation marker 和给 Lua/Agent 使用的 typed host API。V0.4 已实现内存注册表和两个无外部副作用 builtin：`config.lint`、`runtime.echo`。V1.8.5.1 起，manifest 中的 provider/default/fallback metadata 会保存在 descriptor 中，并可生成稳定 provider plan；V1.8.5.2 起，`CapabilityPermissionGate` 会在真实调用前执行 capability、required adapter capability、provider 和 manifest allowlist 门禁；V1.8.5.4 起，`eva-adapter` 的 adapter-backed host 会按 `EvaError::is_retryable()` 对 provider 失败做 fallback 分类；V1.13.3 起，provider admission gate 的 retryable concurrency/rate/circuit 错误也复用该 fallback 分类；V1.15.7 起，`CapabilityHostApi::invoke_with_provider` 为 memory retrieval 提供显式 provider 调用边界，默认实现仍拒绝不支持该能力的 host。真实 Hardware generation/supervision 仍由后续节点接入。

## V0.4 当前实现

| 能力 | 类型/文件 | 当前行为 |
| --- | --- | --- |
| Descriptor | `CapabilityDescriptor` | 可从 manifest 构造，也可构造 builtin descriptor。 |
| Registry | `CapabilityRegistry` | 支持 register、get、list；重复 capability name 返回 `Conflict`。 |
| Selection | `CapabilityProviderSelection` | 生成 explicit request、manifest provider、default provider、fallback providers 的稳定去重顺序。 |
| Gate | `CapabilityPermissionGate` | 默认拒绝未显式授权的 capability/provider，并拒绝 manifest 未声明的 provider。 |
| Builtins | `with_v04_builtins` | 注册 `config.lint` 和 `runtime.echo`。 |
| Router | `CapabilityRouter` | 仅接受 `InvokeTarget::Capability`；builtin 仍本地执行，adapter-backed capability 可先生成 provider plan 或 authorized provider plan。 |
| Host API | `CapabilityHostApi` | 定义 `invoke(InvokeRequest) -> InvokeResponse`，并提供 V1.15.7 `invoke_with_provider` 显式 provider 路由入口。 |
| Generation | `CapabilityGeneration` | 保存 generation id 和 capability count。 |

## Builtin 行为

| Capability | 输出 |
| --- | --- |
| `config.lint` | `{"valid":true,"findings":[],"input":"..."}` |
| `runtime.echo` | `{"echo":"..."}` |

这些 builtin 用于 V0.4 闭环证明，不调用外部 provider，不读取文件，不执行 shell。

## 模块边界

`eva-capability` 不实现 HTTP、stdio、MCP、hardware transport。外部 provider 运行时属于 `eva-adapter`、`eva-mcp`、`eva-hardware` 等后续模块。`selection.rs` 只输出调用顺序和安全元数据，不授予 raw provider/file/socket/process handle；`gate.rs` 只做纯授权检查，不过滤 plan、不启动进程。Lua、Agent 和 memory retrieval 只能通过 `CapabilityHostApi` 间接调用 capability；`invoke_with_provider` 只是显式选择 provider，不授予调用方 raw handle。

## 公开入口

```rust
use eva_capability::{CapabilityHostApi, CapabilityRouter};
```

## 验证

```powershell
cargo test -p eva-capability
```

当前已覆盖：builtin registry、`config.lint` completed response、manifest provider metadata 保留、provider plan 稳定排序、disabled capability 拒绝、未显式授权 capability/provider 拒绝、required adapter capability 拒绝、manifest 外 provider 拒绝，以及 V1.15.7 adapter-backed host 通过 trait 入口执行显式 provider 路由。

## 后续计划

| 版本 | 计划 |
| --- | --- |
| V1.8.5.1 | 已完成 provider selection plan 和稳定 fallback 顺序。 |
| V1.8.5.2 | 已完成 capability/provider permission gate。 |
| V1.8.5.3 | 接入 AdapterRuntime/MCP provider 并统一 InvokeResponse。 |
| V1.8.5.4 | 已完成 retryable fallback 分类和 provider report/error 停止规则。 |
| V1.13.3 | 已验证 retryable provider admission gate 可 fallback，non-retryable 错误仍不 fallback。 |
| V1.15.7 | 已完成 `CapabilityHostApi::invoke_with_provider` 默认拒绝和 adapter-backed host 显式 provider 路由。 |
| V1.3 | 接入 HardwareAdapter provider。 |
