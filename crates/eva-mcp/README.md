# eva-mcp / MCP 协议边界

更新时间：2026-07-09

![V1.x extension module flow](../assets/eva-extension-module-flow.svg)

`eva-mcp` 负责 Model Context Protocol 的 client/server 协议边界、tool/resource/prompt 映射、MCP policy helper 和 schema 约束。它不把内部 Topic 无限制代理给外部 MCP client，也不让外部 MCP server 绕过 AdapterRuntime 和 policy。

## 当前模块功能说明

| 功能域 | 当前状态 | 目标行为 |
| --- | --- | --- |
| Client | 已完成 V1.13.6 | 通过受控 JSON-RPC stdio 或 HTTP transport 连接已配置 MCP server，执行受 allowlist、timeout、output limit 和显式 auth header 限制的 tool 调用。 |
| Server | 已完成受控 surface | `EvaMcpServerSurface` 只暴露显式工具并拒绝无限 Topic、event 和 state 代理；生产 server 数据面仍不在本 crate 中启动。 |
| Tool mapping | 已完成 V1.1 | `McpToolMapping` 和 `McpToolRegistry` 提供确定性 tool-to-capability 映射与重复检测。 |
| Policy helper | 已完成 V1.1 | `McpAllowlist` 校验 tool/resource/prompt allowlist，并在 RPC 前拒绝未授权 tool。 |
| Schema | 已完成基础边界 | `McpSchemaFamily` 固定 tool/resource/prompt/error envelope family，compatibility matrix 负责回归验证。 |
| Compatibility matrix | 已完成 V1.13.7 | 提供 repo-local transport/schema/stream/server-surface fixture，供 release gate 验证。 |
| Discovery 接入 | 已完成候选发现 | `eva-discovery` 从项目 manifest 发现 MCP 候选；发现结果不授予 handle，授权仍由 Adapter/MCP policy gate 决定。 |

## 模块边界

`eva-mcp` 做：

- 表示 MCP client/server 协议数据和 schema。
- 做 MCP tool/resource/prompt 与 Eva capability 的映射。
- 提供 policy helper，帮助 runtime/adapter 判断可调用性。
- 对外暴露受限的 Eva MCP server surface。

`eva-mcp` 不做：

- 不直接启动任意 MCP server。
- 不授予 Adapter 或 capability 执行权。
- 不代理内部 Topic、event log 或 Agent state 给外部 client。
- 不保存长期记忆或 artifact。

## 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V1.1 | 定义 MCP endpoint、tool descriptor、resource descriptor、prompt descriptor。 | MCP 协议版本约束 | schema 可用于 CLI inspect。 |
| 2 | V1.1 | 实现 client abstraction，先支持 mock/in-memory client。 | 标准库或后续 MCP SDK | tool allowlist 测试通过。 |
| 3 | V1.1 | 实现 tool mapping：MCP tool 到 CapabilityRef/Adapter invocation。 | `eva-core`、`eva-capability` | 映射失败有结构化原因。 |
| 4 | V1.1 | 实现 policy helper，生成 request-level PermissionSet 或 gate 输入。 | `eva-policy` | 未授权 tool 返回 PermissionDenied。 |
| 5 | V1.1 | 实现 server surface 初版：`agent.invoke`、`adapter.list`。 | `eva-runtime` 调用方 | server 不暴露无限 Topic 代理。 |
| 6 | V1.1 | 接 Adapter MCP transport。 | `eva-adapter` | Adapter 调用 MCP tool 时可 audit。 |
| 7 | V1.8.2 | 实现 JSON-RPC client transport。 | V1.8.1 provider runner 边界 | fake MCP server tool call、blocked tool、timeout、协议错误和过大响应测试通过。 |
| 8 | V1.8.3 | 增加 server lifecycle 和 session supervisor。 | JSON-RPC client transport | session stop 后无悬挂进程，streaming 可中止，非法代理请求拒绝并审计。 |

## 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 模块导出 | 已完成 V1.8.2 | re-export client、JSON-RPC transport、server、mapping、policy、schema。 |
| `src/client.rs` | in-memory MCP client | 已完成 V1.1 | 保留 CLI probe 和无副作用 envelope 测试。 |
| `src/json_rpc.rs` | MCP JSON-RPC client transport | 已完成 V1.13.6 | 保留 stdio/HTTP JSON-RPC、auth header、allowlist、timeout 和 output-limit 测试；后续接 HTTPS/TLS 和生产 streaming 数据面。 |
| `src/lifecycle.rs` | MCP session registry 和 streaming 边界 | 已完成 V1.8.3 | 保留 stream abort/orphan cleanup 边界；后续接真实 OS process supervisor。 |
| `src/compatibility.rs` | MCP compatibility matrix | 已完成 V1.13.7 | 维护 stdio/HTTP transport、tool schema、stream lifecycle 和 explicit-tool server gate fixture/report。 |
| `src/server.rs` | 受控 MCP server | 已完成 V1.8.3 | 最小 server surface 和 explicit-tool gate 已覆盖；后续接真实 server 数据面。 |
| `src/tool_mapping.rs` | tool-to-capability mapping registry | 已完成 V1.1 | 保持确定性查找和重复 mapping 拒绝。 |
| `src/policy.rs` | MCP allowlist policy helper | 已完成 V1.1 | 在 transport 写入前校验 tool/resource/prompt allowlist。 |
| `src/schema.rs` | MCP schema family 边界 | 已完成基础枚举 | 与 compatibility matrix 一起维护稳定 envelope family。 |
| 单元测试 | mapping/policy/schema/JSON-RPC/lifecycle/compatibility | 已完成 V1.13.7 | 后续增加真实 OS supervisor、HTTPS/TLS 和外部 MCP server compatibility suite。 |

## 验证计划

| 阶段 | 命令 | 目标 |
| --- | --- | --- |
| V1.1 | `cargo test -p eva-mcp` | schema、mapping、policy helper 可测。 |
| V1.1 | `cargo test -p eva-adapter` | MCP transport 只调用 allowlist tool。 |
| V1.8.2 | `cargo test -p eva-mcp -p eva-adapter` | JSON-RPC fake server、blocked tool、timeout、协议错误和 output limit 可测。 |
| V1.8.3 | `cargo test -p eva-mcp -p eva-observability` | session stop 后 registry 无悬挂 session、streaming 可中止、orphan cleanup 和非法代理请求拒绝有稳定 audit。 |
| V1.13.7 | `cargo test -p eva-mcp compatibility -- --nocapture` | compatibility matrix fixture 通过，缺失 cleanup/transport stream lifecycle/无限代理会阻断。 |

## English

`eva-mcp` owns MCP protocol boundaries, client/server surfaces, tool mapping, policy helpers, and schemas. It must not expose unlimited internal Topic or runtime state proxies.

## V1.1 Status

V1.1 implements the MCP safety layer needed by Adapter V1.1 without depending on a real MCP SDK or a running external server:

- `McpAllowlist` validates and stores tool/resource/prompt allowlists, and returns `permission_denied` for unlisted tools.
- `InMemoryMcpClient` supports side-effect-free `probe_tool` and controlled `call_tool` envelopes. It preserves adapter id, request id, tool name, and input text for diagnostics.
- `McpToolMapping` and `McpToolRegistry` provide deterministic tool-to-capability mapping and duplicate mapping detection.
- `EvaMcpServerSurface::v11_minimal()` documents the first server-facing tool surface (`adapter.list`, `adapter.probe`) without opening a socket or stdio server.
- `McpSchemaFamily` names the stable schema envelope families used by future compatibility tests.

V1.8.2 adds a controlled JSON-RPC stdio client transport. It sends `initialize`, `notifications/initialized`, `tools/list`, and `tools/call` requests with generated request ids, blocks non-allowlisted tools before writing RPC, and maps timeout, protocol, JSON-RPC error object, and oversized-response failures into stable `EvaError`s. V1.8.3 and V1.13.6 subsequently added session lifecycle, stream-abort boundaries, HTTP transport, and configured authentication headers.
V1.8.3 adds a session lifecycle registry around the existing supervisor contract. It records started sessions, reports health, removes sessions on shutdown, aborts controlled streams, cleans up missing-process orphans, and rejects non-explicit server tools such as unlimited Topic/event/state proxies with stable audit entries.
V1.13.6 adds an MCP JSON-RPC HTTP client boundary for manifest-selected `http://` MCP endpoints. The client posts `initialize`, `notifications/initialized`, `tools/list`, and `tools/call` over bounded HTTP requests, preserves timeout/output-limit/error-object mapping, sends configured auth headers, and still rejects non-allowlisted tools before any RPC is sent.
V1.13.7 adds `McpCompatibilityMatrix`, a repo-local fixture/report for stdio/HTTP transport, tool schema, stream lifecycle start/abort/cleanup, dangling sessions, and explicit-tool server-surface evidence. It feeds `REL-MCP-COMPAT-001` in `eva-release`. HTTPS/TLS client support, a production streaming data plane, and real external MCP server compatibility certification remain follow-up work.

## P5 Session Boundary

P5 adds a real process/session lifecycle contract without enabling default
runtime process execution:

- `McpSessionConfig` separates `server_transport`, command, args, startup
  timeout, shutdown timeout, and command allowlist.
- `McpSessionManager` builds explicit start/shutdown requests for a
  `McpSessionSupervisor`.
- Tests use a fake supervisor to cover startup failure and shutdown behavior
  without launching an external MCP server.
- `eva-adapter` can derive this typed session config from MCP adapter
  manifests, and V1.8.2 uses it to launch stdio JSON-RPC tool calls.
- `McpSessionRegistry` owns started sessions for V1.8.3 health,
  shutdown, stream-abort, and orphan-cleanup tests.

## V1.1 Verification

```powershell
cargo test -p eva-mcp
cargo run -- mcp list --output json
cargo run -- mcp probe --adapter github-mcp --tool list_issues --output json
cargo run -- mcp probe --adapter github-mcp --tool delete_repo --output json
```
