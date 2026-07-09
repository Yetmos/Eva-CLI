# eva-adapter / 外部能力适配

更新时间：2026-07-09

![V1.x extension module flow](../assets/eva-extension-module-flow.svg)

`eva-adapter` 负责 Adapter manifest 的运行时表示、AdapterRegistry、AdapterRouter、transport runtime 和外部 provider 错误映射。它不做 Discovery 扫描，不授予权限，不改写 policy，只接收已验证配置和已计算边界，并按 transport 约束执行。

V1.1 已实现外部能力的受控 envelope；V1.3 在此基础上实现 hardware transport，使硬件调用必须经由 `eva-hardware` 的 registry、lease 和 driver binding，不允许 Lua 直接访问 raw I/O。V1.8.1 将 stdio/http runner 接入 `AdapterRuntime`，V1.8.2 将 MCP invoke 接到受控 JSON-RPC stdio client，V1.8.4 将 Skill transport 升级为 schema-gated workflow runner；V1.8.5.3 增加 adapter-backed `CapabilityHostApi`，把授权后的 capability plan 接入 `AdapterRuntime` 并统一 `InvokeResponse`；V1.8.5.4 固定 retryable provider fallback 分类；V1.13.1 新增 provider supervisor slot 和 process table evidence。外部 provider 可通过 manifest command/endpoint/env/limits 进入受控执行路径，但完整 OS process supervision 仍是后续能力。

## 当前模块功能说明

| 功能域 | 当前状态 | 已实现行为 |
| --- | --- | --- |
| Manifest runtime | 已完成 V1.8.4 | `AdapterHandle` 从 `AdapterManifest` 派生运行时 handle，保留 transport、capabilities、MCP allowlist、Skill binding/schema/runner、hardware identity，以及 stdio/http command、args、endpoint、env、headers 和 limits。 |
| Registry | 已完成 V1.1 | `AdapterRegistry` 支持按 Adapter id 和 capability 查询，处理重复 provider 和禁用 Adapter。 |
| Router | 已完成 V1.1 | `AdapterRouter` 支持 explicit provider 优先，再按 capability index fallback，并输出结构化错误。 |
| Runtime | 已完成 V1.13.1 | `AdapterRuntime` 提供 list/probe/invoke；probe 无副作用；invoke 可执行 builtin/hardware envelope、受控 stdio/http runner、MCP JSON-RPC stdio tool call，以及 Skill workflow runner；stdio/http/MCP/Skill 调用会先进入 provider supervisor slot 并在 process table 中记录 release evidence。 |
| Capability host | 已完成 V1.8.5.4 | `AdapterBackedCapabilityHost` 复用 `CapabilityRouter::authorized_provider_plan()`，按 provider plan 调用 `AdapterRuntime`，把 report/error 归一为 `InvokeResponse`，并只在 `EvaError::is_retryable()` 为 true 时继续 fallback。 |
| Provider supervisor | 已完成 V1.13.1 | `ProviderSupervisor` / `InMemoryProviderSupervisor` 记录 provider session/process id、manifest digest、start command、health、last error 和 restart policy；unauthorized provider 在 acquire 前失败，transport 启动失败会释放 slot。 |
| Builtin/EventBus/Lua transport | 已完成 V1.1 | 返回本地受控 envelope，不启动外部进程。 |
| MCP transport | 已完成 V1.8.2 | 通过 `eva-mcp::McpJsonRpcClient` 校验 tool allowlist 后启动 manifest stdio server，执行 `initialize`、`tools/list` 和 `tools/call`。 |
| Skill transport | 已完成 V1.8.4 | 校验 `skill.kind == workflow_skill`、`runtime_gate == normal` 和输入 schema；创建隔离 working directory，执行 manifest allowlist process runner 或受控 `codex_skill` runner，保存 stdout/stderr/run-report/artifact evidence 并脱敏 credential 输出。 |
| Hardware transport | 已完成 V1.3 | 通过 `DeviceRegistry` claim/release、`SimulatedDriver` 和 `HardwareDriver` trait 完成模拟硬件调用，audit 包含 `raw_io:false` 和 `lease:released`。 |
| Stdio / HTTP transport | 已完成 V1.13.1 runtime 接入 | Stdio/HTTP 已具备 allowlist、timeout、output limit、manifest command/endpoint/env/limits 读取、credential redaction 和 supervisor process table evidence；并发/限流/熔断留到后续步骤。 |
| MCP process/session | 已完成 V1.8.3 | `AdapterHandle` 从 manifest 读取 `mcp.server_transport`、`mcp.command` 和 `mcp.args`，生成 `eva-mcp::McpSessionConfig`；invoke 可启动 stdio JSON-RPC tool call，session registry 已覆盖 start/health/shutdown/orphan cleanup 和 stream abort 边界。 |

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
- 为 stdio/http/MCP/Skill provider 调用记录 supervisor slot、process table 和 release evidence。
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
| `src/capability_host.rs` | adapter-backed capability host | 已完成 V1.8.5.4 | 已接 authorized provider plan、AdapterRuntime 调用、InvokeResponse completed/failed/timeout 归一化和 retryable fallback 分类；后续补 generation handle 和 provider supervision。 |
| `src/manifest.rs` | Adapter runtime 表示 | 已完成 V1.8.4 | 已包含 MCP session typed config、Skill path/entry/schema/runner/artifact root、hardware identity、stdio/http command/endpoint/env/headers/limits。 |
| `src/registry.rs` | Adapter 注册和索引 | 已完成 V1.1 | 后续接健康 probe、并发限制和熔断状态。 |
| `src/router.rs` | provider 选择 | 已完成 V1.1 | 后续加入优先级和健康降级策略。 |
| `src/runtime.rs` | transport 执行 | 已完成 V1.13.1 | 已接 provider supervisor slot 和 process table evidence；后续补并发限制、熔断和更丰富 provider-specific safe context。 |
| `src/supervisor.rs` | provider supervisor | 已完成 V1.13.1 | 定义 `ProviderSupervisor`、`InMemoryProviderSupervisor`、execution slot acquire/release 和 process table mutation。 |
| `src/error.rs` | 错误映射 | 已完成 V1.1 | 后续扩展 provider-specific safe context。 |
| `src/transports/builtin.rs` | 内置 transport | 已完成 V1.1 | 后续迁移更多内部受控能力。 |
| `src/transports/mcp.rs` | MCP transport | 已完成 V1.8.2 | 已接 MCP JSON-RPC stdio client；真实 session registry、streaming 和 orphan cleanup 后续补齐。 |
| `src/transports/skill.rs` | workflow skill transport | 已完成 V1.8.4 | 已接 schema-gated runner、working directory isolation、artifact evidence、failure/timeout status 和 credential redaction；后续接 runtime worker/policy domain。 |
| `src/transports/hardware.rs` | hardware transport | 已完成 V1.3 | 后续接真实 driver registry 和硬件模拟器测试。 |
| `src/transports/stdio.rs` | stdio 命令 transport | 已完成 V1.8.1 | 已分离 command/args，覆盖 allowlist、timeout、output limit、env 注入和 stdout/stderr 脱敏，并已接入 AdapterRuntime。 |
| `src/transports/http.rs` | HTTP transport | 已完成 V1.8.1 | 已覆盖 URL origin allowlist、method allowlist、timeout、output limit、header env 注入和输出脱敏，并已接入 AdapterRuntime。 |

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
- V1.8.1 覆盖 stdio/http fake provider、disabled provider 不启动、credential env/header/stdout/stderr redaction。
- V1.8.2 覆盖 MCP fake JSON-RPC server tool call、blocked tool 不发 RPC、timeout、协议错误和 oversized response。
- V1.8.4 覆盖 Skill schema required/enum 拒绝、内置 `codex_skill` runner、manifest process runner 成功/失败/超时、artifact path 控制和 credential redaction。
- V1.8.5.3 覆盖 adapter-backed capability host 成功调用、未授权 provider 归一为 failed response、disabled provider 归一为 failed response，以及 timeout report 保留 provider code/context。
- V1.8.5.4 覆盖 retryable provider report failure 后继续 fallback、不可重试配置错误立即停止，以及全部 provider 可重试失败时保留最后一个安全错误。
- V1.13.1 覆盖 provider supervisor acquire/release、process table 查询、disabled provider acquire 前失败，以及 stdio provider 启动失败后释放 slot 并记录 failed audit。

## English

`eva-adapter` owns Adapter runtime descriptors, registry, routing, controlled transport execution, provider error mapping, adapter-backed capability host wiring, and the first provider supervisor slot/process-table baseline. V1.13.1 records session/process evidence for stdio/http/MCP/Skill invocations; full OS process supervision remains future work.
