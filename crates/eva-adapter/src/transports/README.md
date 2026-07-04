# Adapter Transports / Adapter 传输实现

更新时间：2026-07-04

![V1.x extension module flow](../../../assets/eva-extension-module-flow.svg)

本目录承载 Adapter 的具体 transport 实现。所有 transport 都必须由 manifest、effective policy、schema 和 audit gate 约束，禁止变成任意插件、任意 shell、任意 HTTP 或任意硬件代理。

## 当前实现状态

| 文件 | Transport | 当前状态 | 边界 |
| --- | --- | --- | --- |
| `mod.rs` | transport 模块导出 | 已完成 V1.1 | 统一导出具体 transport。 |
| `builtin.rs` | builtin / EventBus / LuaCapability 风格本地能力 | 已完成 V1.1 | 返回本地 JSON envelope，无外部副作用。 |
| `mcp.rs` | MCP tool/resource/prompt | 已完成 V1.1 | 使用 `eva-mcp::McpAllowlist` 和 in-memory client surface，不启动真实 server。 |
| `skill.rs` | workflow skill | 已完成 V1.1 | 校验固定 skill entrypoint 和 `runtime_gate == normal`。 |
| `hardware.rs` | hardware device | 已完成 V1.3 | 通过 `DeviceRegistry` lease 和 `SimulatedDriver`，audit 包含 `raw_io:false`。 |
| `stdio.rs` | stdio command | 仍关闭 | 后续必须 command/args 分离、allowlist、timeout、stdout/stderr 限制。 |
| `http.rs` | HTTP API | 仍关闭 | 后续必须 URL/method/env credential allowlist，不允许任意网络代理。 |
| `eventbus.rs` | EventBus bridge | envelope 阶段 | 内部桥接必须保留 trace 和 runtime ownership。 |
| `lua_capability.rs` | Lua capability bridge | envelope 阶段 | Lua 调用仍必须经过 capability/adapter 边界。 |

## V1.1 受控 envelope

V1.1 已实现可在无外部副作用环境中验证的 transport：

- `builtin.rs`：返回本地 JSON envelope，供内置/EventBus/LuaCapability 风格 transport 使用。
- `mcp.rs`：选择映射的 MCP tool，并在返回 invocation envelope 前强制 `McpAllowlist`。
- `skill.rs`：检查 `skill.runtime_gate == normal`，返回 Skill run envelope 和 audit 记录。

这些实现证明 AdapterRuntime 的调用 envelope、trace、audit 和错误映射可以独立于真实外部进程测试。

## V1.3 hardware transport

`hardware.rs` 是 V1.3 新增的受控硬件 transport。它的步骤是：

1. 从 `AdapterHandle` 读取 `hardware_logical_name` 和 `hardware_device_class`。
2. 构造逻辑 `DeviceCandidate`，并注册到 `DeviceRegistry`。
3. 用 request id claim 设备，获得独占 `DeviceLease`。
4. 用 invocation capability 构造 `DriverBinding`。
5. 调用 `SimulatedDriver`，得到模拟输出和 driver audit。
6. release lease，并把 `transport:hardware`、`lease:released` 加入 audit。

V1.3 的 hardware transport 不打开真实设备，也不读取系统设备路径。真实硬件支持后续必须继续保持这条 registry + lease + driver binding 路径。

## 风险控制规则

| 风险 | 当前控制 |
| --- | --- |
| shell 注入 | stdio transport 未启用；后续必须 command 和 args 分离。 |
| 任意 URL / 凭据泄漏 | HTTP transport 未启用；后续必须 URL 和 env allowlist。 |
| MCP tool 滥用 | MCP transport 只允许 manifest 中 allowlist 的 tool。 |
| workflow skill 越权 | Skill transport 必须通过固定 skill id 和 runtime gate。 |
| raw hardware I/O | hardware transport 只接收 `DeviceLease` 和 `HardwareDriver` 输出，audit 明确 `raw_io:false`。 |
| Lua 绕过 Adapter | Lua 不接收 transport handle；能力调用仍由 AdapterRuntime 分发。 |

## 验证

```powershell
cargo test -p eva-adapter
cargo run -- adapter probe --adapter github-mcp --output json
cargo run -- skill run --skill code-review --input '{"scope":"current_diff"}' --output json
cargo run -- hardware bind --adapter scale-main --output json
```
