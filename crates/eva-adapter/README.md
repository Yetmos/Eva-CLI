# eva-adapter / 外部能力适配

更新时间：2026-07-04

![V1.x extension module flow](../assets/eva-extension-module-flow.svg)

`eva-adapter` 负责 Adapter manifest 的运行时表示、AdapterRegistry、AdapterRouter、transport runtime 和外部 provider 错误映射。它不做 Discovery 扫描，不授予权限，不改写 policy，只接收已验证配置和已计算边界，并按 transport 约束执行。

V1.1 已实现外部能力的受控 envelope；V1.3 在此基础上实现 hardware transport，使硬件调用必须经由 `eva-hardware` 的 registry、lease 和 driver binding，不允许 Lua 直接访问 raw I/O。

## 当前模块功能说明

| 功能域 | 当前状态 | 已实现行为 |
| --- | --- | --- |
| Manifest runtime | 已完成 V1.1/V1.3 | `AdapterHandle` 从 `AdapterManifest` 派生运行时 handle，保留 transport、capabilities、MCP allowlist、Skill binding 和 V1.3 hardware identity。 |
| Registry | 已完成 V1.1 | `AdapterRegistry` 支持按 Adapter id 和 capability 查询，处理重复 provider 和禁用 Adapter。 |
| Router | 已完成 V1.1 | `AdapterRouter` 支持 explicit provider 优先，再按 capability index fallback，并输出结构化错误。 |
| Runtime | 已完成 V1.1/V1.3 | `AdapterRuntime` 提供 list/probe/invoke；probe 无副作用；invoke 只执行已实现的受控 transport。 |
| Builtin/EventBus/Lua transport | 已完成 V1.1 | 返回本地受控 envelope，不启动外部进程。 |
| MCP transport | 已完成 V1.1 | 通过 `eva-mcp::InMemoryMcpClient` 校验 tool allowlist 后返回调用 envelope。 |
| Skill transport | 已完成 V1.1 | 校验 `skill.runtime_gate == normal`，返回带 audit 的 workflow skill envelope。 |
| Hardware transport | 已完成 V1.3 | 通过 `DeviceRegistry` claim/release、`SimulatedDriver` 和 `HardwareDriver` trait 完成模拟硬件调用，audit 包含 `raw_io:false` 和 `lease:released`。 |
| Stdio / HTTP transport | runner contract 已完成，runtime 仍关闭 | Stdio/HTTP 已具备 allowlist、timeout、output limit 的底层 runner；真实 manifest 接入留到后续步骤。 |
| MCP process/session | boundary contract 已完成，runtime 仍不启动 | `AdapterHandle` 从 manifest 读取 `mcp.server_transport`、`mcp.command` 和 `mcp.args`，并生成 `eva-mcp::McpSessionConfig`；当前 invoke 只记录 audit，不启动真实 MCP server。 |

## V1.3 Hardware Transport

hardware transport 的调用路径：

1. `AdapterHandle::from_manifest` 从 hardware manifest 读取 `hardware.identity.logical_name` 和 `hardware.identity.device_class`。
2. `AdapterRuntime::invoke` 对 `AdapterTransport::Hardware` 分发到 `transports::hardware::invoke`。
3. transport 构造受控 `DeviceCandidate`，注册到 `DeviceRegistry`。
4. registry 用 request id claim 设备，生成独占 `DeviceLease`。
5. `DriverBinding` 将 Adapter capability 绑定到 simulated driver。
6. `SimulatedDriver` 返回模拟读取结果，audit 明确 `raw_io:false`。
7. registry release lease，transport 返回 `AdapterInvokeReport`，audit 补充 `transport:hardware` 和 `lease:released`。

V1.3 的 hardware transport 只证明边界：它不会打开真实 USB、串口、BLE、网络 socket、设备路径或 vendor SDK。真实 driver 后续必须继续使用 `DeviceLease` 和 `HardwareDriver` trait，不能把 raw handle 暴露给 Lua 或 Agent。

## 模块边界

`eva-adapter` 做：

- 把已授权调用转换为具体 transport invocation。
- 管理 Adapter 的注册、健康状态、provider 索引。
- 对外部错误做脱敏、分类和可重试标记。
- 记录 audit、trace、transport 输出。
- 在 hardware transport 中只经 `eva-hardware` 访问设备 lease 和 driver binding。

`eva-adapter` 不做：

- 不扫描本地命令、MCP server 或项目 manifests，这属于 `eva-discovery` 和 `eva-config`。
- 不决定权限上限，这属于 `eva-policy`。
- 不让 Adapter 任意读取 workspace、shell、网络或密钥。
- 不直接变成通用插件系统，所有 transport 必须有 manifest 和 schema。
- 不向 Lua 暴露硬件设备路径、文件描述符、串口句柄或 SDK client。

## 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 模块导出 | 已完成 V1.1 | 后续按 transport 稳定性拆分公共 surface。 |
| `src/manifest.rs` | Adapter runtime 表示 | 已完成 P5 | 已包含 MCP session typed config；后续加入更多 transport-specific typed config。 |
| `src/registry.rs` | Adapter 注册和索引 | 已完成 V1.1 | 后续接健康 probe、并发限制和熔断状态。 |
| `src/router.rs` | provider 选择 | 已完成 V1.1 | 后续加入优先级和健康降级策略。 |
| `src/runtime.rs` | transport 执行 | 已完成 V1.3 | 后续把 stdio/http runner 接入 manifest typed config。 |
| `src/error.rs` | 错误映射 | 已完成 V1.1 | 后续扩展 provider-specific safe context。 |
| `src/transports/builtin.rs` | 内置 transport | 已完成 V1.1 | 后续迁移更多内部受控能力。 |
| `src/transports/mcp.rs` | MCP transport | 已完成 P5 | 已接 MCP process/session config 边界；真实 server 启动仍需显式 supervisor 接入。 |
| `src/transports/skill.rs` | workflow skill transport | 已完成 V1.1 | 后续接 runtime worker，但保持 gate 和 audit。 |
| `src/transports/hardware.rs` | hardware transport | 已完成 V1.3 | 后续接真实 driver registry 和硬件模拟器测试。 |
| `src/transports/stdio.rs` | stdio 命令 transport | runner contract 已完成 | 已分离 command/args，并覆盖 allowlist、timeout、output limit；后续接入 AdapterRuntime。 |
| `src/transports/http.rs` | HTTP transport | runner contract 已完成 | 已覆盖 URL origin allowlist、method allowlist、timeout 和 output limit；后续接入 AdapterRuntime。 |

## 验证计划

```powershell
cargo test -p eva-adapter
cargo run -- adapter list --output json
cargo run -- adapter probe --adapter github-mcp --output json
cargo run -- skill run --skill code-review --input '{"scope":"current_diff"}' --output json
cargo run -- hardware bind --adapter scale-main --output json
```

V1.3 关键测试覆盖：

- `AdapterHandle` 能读取 hardware identity 扩展字段。
- `AdapterRuntime` 对 hardware transport 进入 `transports::hardware`。
- hardware transport 输出 simulated audit，包含 `raw_io:false` 和 `lease:released`。
- 禁用的 `scale-main` 在 CLI bind 命令中保持 blocked/plan-first，不打开设备。

## English

`eva-adapter` owns Adapter runtime descriptors, registry, routing, controlled transport execution, and provider error mapping. V1.3 adds a hardware transport boundary backed by `eva-hardware` registry leases and a simulated driver; raw hardware I/O remains closed.
