# eva-adapter / 外部能力适配

更新时间：2026-07-02

![V1.x extension module flow](../assets/eva-extension-module-flow.svg)

`eva-adapter` 负责 Adapter manifest 的运行时表示、AdapterRegistry、AdapterRouter、transport runtime 和外部 provider 错误映射。它不做 Discovery 扫描，不授予权限，不改写 policy，只接收已计算的 effective policy 并按约束执行。

## 当前模块功能说明

| 功能域 | 当前状态 | 目标行为 |
| --- | --- | --- |
| Manifest runtime | 骨架 | 将 `eva-config` AdapterManifest 转成 runtime descriptor。 |
| Registry | 骨架 | 注册 Adapter handle、capability index、health、policy requirement。 |
| Router | 骨架 | 按 capability/provider 显式请求选择 Adapter。 |
| Runtime | 骨架 | 执行 transport，处理 timeout、audit、trace、结构化错误。 |
| Error mapping | 骨架 | 把 provider/transport 错误映射成稳定 `EvaError`。 |
| Builtin transport | 骨架 | 提供本地内置能力入口。 |
| Stdio transport | 骨架 | 执行 allowlist 中命令，命令和参数分离。 |
| HTTP transport | 骨架 | 通过 manifest 和 env allowlist 获取凭据，禁止任意 URL。 |
| MCP transport | 骨架 | 调用受 allowlist 限制的 MCP tool/resource/prompt。 |
| Skill transport | 骨架 | 调用受控 workflow skill 入口。 |
| Hardware transport | 骨架 | 通过 `eva-hardware` 设备 handle 访问硬件。 |
| EventBus/Lua transport | 骨架 | 内部能力桥接，不绕过 policy gate。 |

## 模块边界

`eva-adapter` 做：

- 把已授权调用转换为具体 transport invocation。
- 管理 Adapter 的注册、健康状态、provider 索引。
- 对外部错误做脱敏、分类和可重试标记。
- 记录 audit、trace、metrics。

`eva-adapter` 不做：

- 不扫描本地命令、MCP server 或项目 manifests，这属于 `eva-discovery` 和 `eva-config`。
- 不决定权限上限，这属于 `eva-policy`。
- 不让 Adapter 任意读取 workspace、shell、网络或密钥。
- 不直接变成通用插件系统，所有 transport 必须有 manifest 和 schema。

## 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V1.1 | 定义 runtime `AdapterDescriptor`、capability index、health summary。 | `eva-config`、`eva-core` | manifest 可转 runtime descriptor。 |
| 2 | V1.1 | 实现 AdapterRegistry add/remove/list/get 和重复 provider 检查。 | 标准库 | 重复 ID、禁用 Adapter 可测。 |
| 3 | V1.1 | 实现 AdapterRouter：explicit provider 优先，capability index 兜底。 | `eva-capability` | 路由结果可解释和可审计。 |
| 4 | V1.1 | 实现 runtime invocation envelope、timeout、audit、error mapping。 | `eva-policy`、`eva-observability` | PermissionDenied、timeout、provider error 可区分。 |
| 5 | V1.1 | 实现 builtin、stdio、HTTP transport 初版。 | manifest policy | 无 manifest 的命令/URL 不能执行。 |
| 6 | V1.1 | 实现 MCP 和 Skill transport 适配。 | `eva-mcp`、workflow skill manifest | tool 和 skill 只能经 allowlist 调用。 |
| 7 | V1.3 | 实现 hardware transport。 | `eva-hardware` | 设备调用不暴露 raw I/O 给 Lua。 |
| 8 | V1.5 | 增加 transport metrics、并发限制、熔断和健康 probe。 | runtime observability | provider 故障可降级。 |

## 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 模块导出 | 骨架 | re-export descriptor、registry、router、runtime、transport。 |
| `src/manifest.rs` | Adapter runtime 表示 | `RESPONSIBILITY` 占位 | 定义 descriptor、transport config、schema ref。 |
| `src/registry.rs` | Adapter 注册和索引 | `RESPONSIBILITY` 占位 | 实现按 ID、capability、provider 查询。 |
| `src/router.rs` | provider 选择 | `RESPONSIBILITY` 占位 | 实现 explicit provider、fallback、冲突错误。 |
| `src/runtime.rs` | transport 执行 | `RESPONSIBILITY` 占位 | 实现 invocation envelope、timeout、audit。 |
| `src/error.rs` | 错误映射 | `RESPONSIBILITY` 占位 | 定义 provider code、retryable、safe context。 |
| `src/transports/builtin.rs` | 内置 transport | 骨架 | 实现 no-op/demo provider。 |
| `src/transports/stdio.rs` | stdio 命令 transport | 骨架 | 分离 command/args，禁止 shell string。 |
| `src/transports/http.rs` | HTTP transport | 骨架 | manifest URL allowlist 和 env credential allowlist。 |
| `src/transports/mcp.rs` | MCP transport | 骨架 | 映射到 `eva-mcp` client。 |
| `src/transports/skill.rs` | workflow skill transport | 骨架 | 只允许固定 skill entrypoint。 |
| `src/transports/hardware.rs` | hardware transport | 骨架 | 通过 DeviceRegistry handle 调用。 |
| `src/transports/eventbus.rs` | EventBus bridge | 骨架 | 内部事件桥接时保留 trace。 |
| `src/transports/lua_capability.rs` | Lua capability bridge | 骨架 | Lua 到 capability 的受控桥。 |
| `src/README.md` | 源码目录说明 | 简略 | 补充文件职责和进度。 |
| `src/transports/README.md` | transport 目录说明 | 简略 | 补充各 transport 的 policy gate。 |

## 验证计划

| 阶段 | 命令 | 目标 |
| --- | --- | --- |
| V1.1 | `cargo test -p eva-adapter` | registry、router、runtime、error mapping 可测。 |
| V1.1 | Adapter integration tests | stdio/http/mcp/skill transport policy gate 可测。 |
| V1.3 | Hardware adapter tests | 设备调用只能通过 hardware transport。 |

## English

`eva-adapter` owns Adapter runtime descriptors, registry, routing, transport execution, and provider error mapping. Discovery and authorization are separate responsibilities.

## V1.1 Status

V1.1 turns the former placeholder crate into a side-effect-safe external capability control layer. The crate now provides:

- `AdapterHandle`: a runtime handle derived only from validated `AdapterManifest` data, including transport, exposed capabilities, source path, MCP tool allowlist, and Skill binding metadata.
- `AdapterRegistry`: deterministic lookup by Adapter id and by `CapabilityName`, with duplicate id detection and capability-provider indexing.
- `AdapterRouter`: explicit provider routing first, then capability index fallback; disabled or mismatched providers fail with structured `EvaError` values.
- `AdapterRuntime`: list/probe/invoke APIs used by CLI V1.1 commands. Probe is side-effect free. Invoke currently supports controlled envelopes for builtin/Lua/EventBus, MCP, and Skill transports.
- `transports/mcp.rs`: calls `eva-mcp::InMemoryMcpClient` so only allowlisted tools can be invoked.
- `transports/skill.rs`: validates `skill.runtime_gate == normal` and returns an auditable controlled envelope instead of launching an arbitrary workflow runner.

V1.1 intentionally does not start real stdio/http/hardware providers. Those transports return structured `unsupported` diagnostics until later versions add process execution, URL allowlists, hardware handles, and richer policy evaluation.

## V1.1 Verification

```powershell
cargo test -p eva-adapter
cargo run -- adapter list --output json
cargo run -- adapter probe --adapter github-mcp --output json
cargo run -- skill run --skill code-review --input '{"scope":"current_diff"}' --output json
```

## V1.1 Status

V1.1 turns the former placeholder crate into a side-effect-safe external capability control layer. The crate now provides:

- `AdapterHandle`: a runtime handle derived only from validated `AdapterManifest` data, including transport, exposed capabilities, source path, MCP tool allowlist, and Skill binding metadata.
- `AdapterRegistry`: deterministic lookup by Adapter id and by `CapabilityName`, with duplicate id detection and capability-provider indexing.
- `AdapterRouter`: explicit provider routing first, then capability index fallback; disabled or mismatched providers fail with structured `EvaError` values.
- `AdapterRuntime`: list/probe/invoke APIs used by CLI V1.1 commands. Probe is side-effect free. Invoke currently supports controlled envelopes for builtin/Lua/EventBus, MCP, and Skill transports.
- `transports/mcp.rs`: calls `eva-mcp::InMemoryMcpClient` so only allowlisted tools can be invoked.
- `transports/skill.rs`: validates `skill.runtime_gate == normal` and returns an auditable controlled envelope instead of launching an arbitrary workflow runner.

V1.1 intentionally does not start real stdio/http/hardware providers. Those transports return structured `unsupported` diagnostics until later versions add process execution, URL allowlists, hardware handles, and richer policy evaluation.

## V1.1 Verification

```powershell
cargo test -p eva-adapter
cargo run -- adapter list --output json
cargo run -- adapter probe --adapter github-mcp --output json
cargo run -- skill run --skill code-review --input '{"scope":"current_diff"}' --output json
```
