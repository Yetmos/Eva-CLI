# eva-adapter/src / Adapter 源码

更新时间：2026-07-04

![V1.x extension module flow](../../assets/eva-extension-module-flow.svg)

本目录承载 Adapter runtime descriptor、registry、router、transport runtime 和错误映射。V1.1 已经把外部能力做成 side-effect-safe 的受控 envelope；V1.3 新增 hardware transport，让硬件能力经由 `eva-hardware` 的 registry lease 和 driver binding 执行。

## 文件职责

| 文件/目录 | 职责 | 当前进度 | 说明 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 已完成 V1.1 | 导出 manifest、registry、router、runtime、error。 |
| `manifest.rs` | Adapter manifest 的 runtime 表示 | 已完成 V1.3 | `AdapterHandle` 保留 MCP、Skill 和 hardware identity 扩展。 |
| `registry.rs` | Adapter handle 和 capability index | 已完成 V1.1 | 支持按 id/capability 查询和重复检测。 |
| `router.rs` | explicit provider 和 capability 路由 | 已完成 V1.1 | provider 优先，fallback 到 capability index。 |
| `runtime.rs` | 授权后 transport 执行、probe、audit | 已完成 V1.3 | hardware transport 已接入；stdio/http 仍返回 unsupported。 |
| `error.rs` | provider/transport 错误映射 | 已完成 V1.1 | 稳定输出 permission/unavailable/unsupported/conflict 等错误。 |
| `transports/` | 具体 transport 实现 | 已完成 V1.1/V1.3 部分 | builtin/MCP/Skill/hardware 有受控实现；stdio/http 关闭。 |

## V1.1 已实现 surface

- `manifest.rs`：定义 `AdapterHandle`、`AdapterHealth`、`AdapterCapabilityBinding`。
- `registry.rs`：按 Adapter id 和 capability 建立索引。
- `router.rs`：支持 explicit provider routing 和 capability-index fallback。
- `runtime.rs`：提供 `AdapterRuntime::from_project`、`list`、`probe_adapter`、`probe_capability`、`invoke`。
- `transports/builtin.rs`：返回本地受控 envelope。
- `transports/mcp.rs`：通过 `eva-mcp` 强制 MCP tool allowlist。
- `transports/skill.rs`：校验 Skill runtime gate 并返回可审计 envelope。

V1.1 不启动 stdio/http/hardware provider。这保证外部执行先有 manifest、policy、audit、credential、timeout 和平台边界。

## V1.3 新增 surface

- `manifest.rs` 新增 `hardware_logical_name` 和 `hardware_device_class`，来源是 Adapter manifest 的 `hardware.identity.*` 扩展字段。
- `runtime.rs` 将 `AdapterTransport::Hardware` 路由到 `transports::hardware::invoke`。
- `transports/hardware.rs` 通过 `DeviceRegistry` claim/release 设备，使用 `DriverBinding` 和 `SimulatedDriver` 执行模拟硬件读取。
- hardware audit 输出 `raw_io:false`、`transport:hardware`、`lease:released`，证明 V1.3 没有打开真实设备。

## Transport 状态

| Transport | 状态 | 风险控制 |
| --- | --- | --- |
| builtin | 已完成 V1.1 | 仅本地 envelope。 |
| eventbus | 已完成 V1.1 envelope | 继续保留 trace，不绕过 runtime。 |
| lua_capability | 已完成 V1.1 envelope | Lua 仍走 capability/adapter 边界。 |
| mcp | 已完成 V1.1 | tool allowlist 先于 envelope。 |
| skill | 已完成 V1.1 | runtime gate 必须为 `normal`。 |
| hardware | 已完成 V1.3 | 只接受 registry lease 和 driver binding，不暴露 raw I/O。 |
| stdio | 仍关闭 | 后续必须 command/args 分离并强制 allowlist。 |
| http | 仍关闭 | 后续必须 URL、method、credential allowlist。 |

## 验证

```powershell
cargo test -p eva-adapter
cargo run -- adapter list --output json
cargo run -- adapter probe --adapter github-mcp --output json
cargo run -- hardware bind --adapter scale-main --output json
```

当前测试覆盖 registry/router/runtime、MCP allowlist、Skill gate、hardware identity 读取和 hardware transport simulated audit。
