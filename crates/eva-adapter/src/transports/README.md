# Adapter Transports / Adapter 传输实现

更新时间：2026-07-09

![V1.x extension module flow](../../../assets/eva-extension-module-flow.svg)

本目录承载 Adapter 的具体 transport 实现。所有 transport 都必须由 manifest、effective policy、schema 和 audit gate 约束，禁止变成任意插件、任意 shell、任意 HTTP 或任意硬件代理。

## 当前实现状态

| 文件 | Transport | 当前状态 | 边界 |
| --- | --- | --- | --- |
| `mod.rs` | transport 模块导出 | 已完成 V1.1 | 统一导出具体 transport。 |
| `builtin.rs` | builtin / EventBus / LuaCapability 风格本地能力 | 已完成 V1.1 | 返回本地 JSON envelope，无外部副作用。 |
| `mcp.rs` | MCP tool/resource/prompt | 已完成 V1.13.6 | 使用 `eva-mcp::McpJsonRpcClient`，按 manifest MCP session config 启动 stdio JSON-RPC server，或调用 manifest-selected `http://` MCP endpoint；发送前执行 tool allowlist、provider credential session/auth header gate、timeout/output-limit/error-object mapping，result 以 stream summary 输出并写入 redacted artifact。 |
| `skill.rs` | workflow skill | 已完成 V1.13.4 | 校验固定 skill id、`kind == workflow_skill`、`runtime_gate == normal` 和 input schema；执行受控 workflow runner，保存 artifact evidence，并脱敏 credential/session token；provider admission gate 先于 runner 启动，stdout/stderr 以 stream summary 输出。 |
| `hardware.rs` | hardware device | 已完成 V1.3 | 通过 `DeviceRegistry` lease 和 `SimulatedDriver`，audit 包含 `raw_io:false`。 |
| `stdio.rs` | stdio command | 已完成 V1.13.4 | 已实现 command/args 分离、allowlist、timeout、stdout/stderr 限制、credential/session env 注入、脱敏、provider admission gate 和 stream artifact summary，并接入 AdapterRuntime。 |
| `http.rs` | HTTP API | 已完成 V1.13.4 | 已实现 URL origin allowlist、method allowlist、timeout、output limit、credential/session header 注入、chunked body read、输出脱敏、provider admission gate 和 stream artifact summary，并接入 AdapterRuntime。 |
| `eventbus.rs` | EventBus bridge | envelope 阶段 | 内部桥接必须保留 trace 和 runtime ownership。 |
| `lua_capability.rs` | Lua capability bridge | envelope 阶段 | Lua 调用仍必须经过 capability/adapter 边界。 |

## V1.1 受控 envelope

V1.1 已实现可在无外部副作用环境中验证的 transport：

- `builtin.rs`：返回本地 JSON envelope，供内置/EventBus/LuaCapability 风格 transport 使用。
- `mcp.rs`：选择映射的 MCP tool，在写入 JSON-RPC 前强制 `McpAllowlist`，并通过 stdio transport 或受限 `http://` transport 执行 `initialize`、`tools/list`、`tools/call`；HTTP MCP 调用必须携带 provider session auth header。
- `skill.rs`：V1.8.4 起检查 `skill.kind`、`skill.runtime_gate` 和 input schema，创建隔离 working directory，执行 manifest allowlist process runner 或受控 `codex_skill` runner，并保存 stdout/stderr/run-report/artifact evidence。

这些实现证明 AdapterRuntime 的调用 envelope、trace、audit 和错误映射可以用 fake provider 和受控 runner 测试。V1.13.7 后 stdio/http、MCP stdio/HTTP JSON-RPC tool call 和 Skill workflow runner 已能通过 AdapterRuntime 进入 supervisor slot、credential session scope、concurrency/rate/circuit admission gate、bounded stream artifact 数据面、durable provider process recovery evidence 和 MCP compatibility matrix release gate；MCP 生产 streaming/TLS/真实外部 server compatibility、OS process supervisor、OS credential vault 和 Skill output schema 深度校验会在后续节点补齐。

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
| shell 注入 | stdio runner 不走 shell，command 和 args 分离，并要求 command allowlist。 |
| 任意 URL / 凭据泄漏 | HTTP runner 要求 URL origin allowlist 和 method allowlist；credential env/header 和 provider session token 注入会在输出、artifact 和 audit 中脱敏。 |
| MCP tool 滥用 | MCP transport 只允许 manifest 中 allowlist 的 tool。 |
| MCP server 生命周期失控 | MCP invoke 单次 stdio JSON-RPC tool call 会在结束后停止进程；V1.8.3 已补 session registry、stream abort 和 orphan cleanup 边界。 |
| workflow skill 越权 | Skill transport 必须通过固定 skill id、runtime gate、input schema、manifest command allowlist 和受控 artifact path。 |
| raw hardware I/O | hardware transport 只接收 `DeviceLease` 和 `HardwareDriver` 输出，audit 明确 `raw_io:false`。 |
| Lua 绕过 Adapter | Lua 不接收 transport handle；能力调用仍由 AdapterRuntime 分发。 |

## 验证

```powershell
cargo test -p eva-adapter
cargo run -- adapter probe --adapter github-mcp --output json
cargo run -- skill run --skill code-review --input '{"scope":"current_diff"}' --output json
cargo run -- hardware bind --adapter scale-main --output json
```
