# eva-adapter/src / Adapter 源码

更新时间：2026-07-09

![V1.x extension module flow](../../assets/eva-extension-module-flow.svg)

本目录承载 Adapter runtime descriptor、registry、router、transport runtime、provider supervisor、adapter-backed capability host 和错误映射。V1.1 已经把外部能力做成 side-effect-safe 的受控 envelope；V1.3 新增 hardware transport，让硬件能力经由 `eva-hardware` 的 registry lease 和 driver binding 执行；V1.8.1 将 stdio/http runner 接入 `AdapterRuntime`，V1.8.2 将 MCP invoke 接到 JSON-RPC stdio client，V1.8.4 将 Skill transport 接到 schema-gated workflow runner，V1.8.5.4 将授权后的 capability provider plan 接到 `AdapterRuntime`，统一 `InvokeResponse`，并按 retryable 分类执行 fallback；V1.13.1 新增 provider supervisor slot 和 process table evidence；V1.13.2 新增 provider credential session scope；V1.13.3 新增 provider concurrency/rate/circuit admission gate；V1.13.4 新增 provider stream artifact 数据面；V1.13.5 新增 durable provider process table mirror 入口。

## 文件职责

| 文件/目录 | 职责 | 当前进度 | 说明 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 已完成 V1.13.5 | 导出 manifest、registry、router、runtime、supervisor、error。 |
| `capability_host.rs` | adapter-backed capability host | 已完成 V1.8.5.4 | 复用 capability authorized provider plan，调用 `AdapterRuntime`，把 `AdapterInvokeReport` 和 transport error 归一为 `InvokeResponse`，并只在 `EvaError::is_retryable()` 为 true 时继续 fallback。 |
| `manifest.rs` | Adapter manifest 的 runtime 表示 | 已完成 V1.8.4 | `AdapterHandle` 保留 MCP、Skill path/entry/schema/runner/artifact root、hardware identity 以及 stdio/http command、args、endpoint、env、headers、limits 扩展。 |
| `registry.rs` | Adapter handle 和 capability index | 已完成 V1.1 | 支持按 id/capability 查询和重复检测。 |
| `router.rs` | explicit provider 和 capability 路由 | 已完成 V1.1 | provider 优先，fallback 到 capability index。 |
| `runtime.rs` | 授权后 transport 执行、probe、audit | 已完成 V1.13.5 | provider invocation report 包含 request-level `TraceFields`；hardware、stdio、http、MCP JSON-RPC 和 Skill workflow runner 已接入；stdio/http/MCP/Skill 调用会先进入 provider supervisor slot，绑定 credential session scope，执行 admission gate，并释放到 process table；可选 filesystem provider process table mirror 供 daemon recovery 扫描。 |
| `supervisor.rs` | provider supervisor | 已完成 V1.13.5 | 定义 `ProviderSupervisor`、`InMemoryProviderSupervisor`、`ProviderCredentialScope`、execution slot acquire/release、process table mutation、concurrency/rate/circuit admission、half-open probe 和 durable process table mirror。 |
| `error.rs` | provider/transport 错误映射 | 已完成 V1.1 | 稳定输出 permission/unavailable/unsupported/conflict 等错误。 |
| `transports/` | 具体 transport 实现 | 已完成 V1.8.4 | builtin/hardware 有受控实现；stdio/http runner、MCP JSON-RPC stdio tool call 和 Skill schema-gated runner 已接入 runtime，带 manifest command/endpoint/env/limits、timeout、output limit、artifact evidence 和 credential redaction。 |

## V1.1 已实现 surface

- `manifest.rs`：定义 `AdapterHandle`、`AdapterHealth`、`AdapterCapabilityBinding`。
- `registry.rs`：按 Adapter id 和 capability 建立索引。
- `router.rs`：支持 explicit provider routing 和 capability-index fallback。
- `runtime.rs`：提供 `AdapterRuntime::from_project`、`list`、`probe_adapter`、`probe_capability`、`invoke`。
- `transports/builtin.rs`：返回本地受控 envelope。
- `transports/mcp.rs`：通过 `eva-mcp` 强制 MCP tool allowlist。
- `transports/skill.rs`：V1.8.4 起校验 Skill runtime gate/input schema，并执行受控 workflow runner。

P5 provider invocation reports attach `TraceFields` with request id, adapter id,
capability, provider, and the stable `adapter.invoke` span. CLI JSON can
therefore show transport audit entries and invocation trace in the same data
object.

V1.8.1 起 `AdapterRuntime` 可以启动 stdio/http provider runner。stdio 子进程不走 shell，只允许 manifest `command`；HTTP 先支持标准库 `http://` fake/明文 provider，`https://` 在没有 TLS client 时返回稳定 unsupported。V1.8.2 起 MCP transport 会按 manifest `mcp.command`/`mcp.args` 启动 stdio JSON-RPC server，完成 `initialize`、`tools/list`、`tools/call`，并在发送前拦截未授权 tool。V1.8.4 起 Skill transport 会校验 manifest input schema、创建隔离 working directory、执行 manifest allowlist process runner 或受控 `codex_skill` runner，并把 stdout/stderr/run-report/artifact 写入 filesystem artifact store。credential env/header 只进入受控 runner，输出和审计默认脱敏。

V1.13.1 起 stdio、http、MCP 和 Skill transport 在 dispatch 前通过 `ProviderSupervisor` 申请 execution slot。`ProviderProcessSnapshot` 会记录 provider session/process id、manifest digest、start command、health、last error、restart policy 和 retry backoff hint；disabled/unauthorized provider 在 slot acquire 前失败，transport 启动失败也会释放 slot 并记录 failed audit。V1.13.4 已补 bounded stream summary 和 redacted artifact 数据面；V1.13.5 可通过 filesystem provider process table mirror 让 daemon restart recovery 扫描残留 active session。该基线仍不等于 OS process supervisor、完整 MCP HTTP/auth/streaming lifecycle 或真实 OS 进程崩溃管理。

V1.13.2 起 `ProviderCredentialScope` 将 session token 绑定到 provider/session/request/capability。`AdapterRuntime` 拒绝 caller-supplied scope，并通过 `provider.credential_session` policy decision 记录 allow/deny audit；stdio/HTTP/Skill 会注入 scoped env/header/token 并把 token 纳入 stdout/stderr/body/artifact redaction；跨 provider/session 复用会在 runner 启动前拒绝。该基线仍不等于 OS credential vault 或真实进程用户隔离。

V1.13.3 起 `ProviderExecutionRequest` 携带 manifest concurrency/rate/circuit limit 和 policy retry backoff。`InMemoryProviderSupervisor` 在 acquire 前执行 admission：并发超限、rate window 耗尽和 circuit open 均返回 retryable `Unavailable` 且不启动新进程；失败达到阈值时 provider health 进入 `circuit_open`，恢复窗口后允许 half-open probe。

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
| mcp | 已完成 V1.13.4 | tool allowlist 先于 JSON-RPC；响应受 timeout 和 output limit 约束；调用进入 supervisor slot 并记录 credential session audit；provider admission gate 先于进程启动；result 以 stream summary 输出并写入 redacted artifact。 |
| skill | 已完成 V1.13.4 | runtime gate 必须为 `normal`；输入 schema、working directory、artifact path、credential/session token redaction、provider session scope、admission gate 和 stdout/stderr stream summary 受控。 |
| hardware | 已完成 V1.3 | 只接受 registry lease 和 driver binding，不暴露 raw I/O。 |
| stdio | 已完成 V1.13.4 | command/args 分离，强制 allowlist，覆盖 timeout、output limit、credential/session env 注入、stdout/stderr 脱敏、supervisor admission、release evidence 和 stream artifact summary。 |
| http | 已完成 V1.13.4 | URL origin allowlist、method allowlist、timeout、output limit、credential/session header 注入、输出脱敏、chunked body read、supervisor admission、release evidence 和 stream artifact summary 已覆盖。 |

## V1.13.4 Provider Stream Data Plane

- `stream.rs` 统一 bounded chunk capture、preview、truncation、redaction 和 artifact evidence。
- stdio/Skill stdout/stderr、HTTP body 和 MCP result 的 invoke JSON 只输出 stream summary。
- 完整受限 stream 写入受控 `FileSystemArtifactStore` key；HTTP TCP client 不再用 `read_to_end` 读取完整响应。

## V1.13.5 Provider Process Mirror

- `InMemoryProviderSupervisor::with_process_table()` 可把 acquire/complete snapshot 同步写入 `FileSystemProviderProcessTable`。
- `AdapterRuntime::from_project_with_provider_process_table()` 和 `from_registry_with_provider_process_table()` 暴露受控构造入口。
- 该 mirror 只提供 restart recovery evidence，不负责启动、杀死或重启 OS 进程。

## 验证

```powershell
cargo test -p eva-adapter
cargo run -- adapter list --output json
cargo run -- adapter probe --adapter github-mcp --output json
cargo run -- hardware bind --adapter scale-main --output json
```

当前测试覆盖 registry/router/runtime、adapter-backed capability host、retryable provider fallback、non-retryable provider stop、retryable rate-limit gate fallback、MCP allowlist、MCP JSON-RPC fake server call、blocked tool 不发 RPC、timeout/protocol/output-limit 错误、Skill schema gate/runner/artifact evidence/credential redaction、hardware identity 读取、hardware transport simulated audit、stdio runtime runner/redaction/disabled-provider gate、stdio runner denied command/timeout/output limit、HTTP URL allowlist、method denial、timeout、runtime fake provider、credential header redaction、V1.13.1 provider supervisor acquire/release、process table 查询和 failed provider slot release、V1.13.2 credential session token redaction 和跨 provider scope 拒绝、V1.13.3 concurrency/rate/circuit admission 和 half-open probe、V1.13.4 provider stream summary/artifact truncation，以及 V1.13.5 durable provider process table mirror。
