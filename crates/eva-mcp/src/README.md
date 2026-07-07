# eva-mcp/src / MCP 源码

![V1.x extension module flow](../../assets/eva-extension-module-flow.svg)

## V1.1 Implemented Surface

- `policy.rs`: `McpAllowlist` validates tool/resource/prompt names and blocks unlisted tools.
- `client.rs`: `InMemoryMcpClient` supports safe probe and controlled call envelopes.
- `json_rpc.rs`: `McpJsonRpcClient` drives stdio JSON-RPC `initialize`, `tools/list`, and `tools/call` with allowlist, timeout, output-limit, and protocol-error gates.
- `lifecycle.rs`: `McpSessionRegistry` owns started sessions, health reports, stream aborts, shutdown removal, and orphan cleanup.
- `tool_mapping.rs`: `McpToolMapping` and `McpToolRegistry` provide deterministic mapping and duplicate checks.
- `server.rs`: `EvaMcpServerSurface::v11_minimal()` documents side-effect-free server tool exposure.
- `schema.rs`: `McpSchemaFamily` names stable envelope families for later compatibility work.
- `session.rs`: `McpSessionConfig`, `McpSessionManager`, and `McpSessionSupervisor` define explicit MCP process startup and shutdown requests.

The V1.1/P5 MCP crate started as a protocol/control boundary. V1.8.2 adds a real stdio JSON-RPC client path for adapter invocation while keeping CLI probe side-effect-free. V1.8.3 adds a session registry and explicit-tool server gate; real OS process supervision and streaming data remain later work.

本目录承载 MCP client/server、tool mapping、policy helper 和 schema 边界。当前为骨架，V1.1 先实现受 allowlist 限制的 client/mapping 和受控 server surface。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 骨架 | V1.1 |
| `client.rs` | MCP client protocol integration | 骨架 | V1.1 |
| `json_rpc.rs` | MCP JSON-RPC stdio client transport | 已完成 | V1.8.2 |
| `lifecycle.rs` | MCP session registry、health、stream abort、orphan cleanup | 已完成 | V1.8.3 |
| `server.rs` | 受控 MCP server exposure | 已完成 V1.8.3 | V1.1/V1.8.3 |
| `tool_mapping.rs` | tool/resource/prompt 到 capability 的映射 | 骨架 | V1.1 |
| `policy.rs` | MCP policy helper | 骨架 | V1.1 |
| `schema.rs` | MCP 输入输出 schema 边界 | 骨架 | V1.1 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义 MCP descriptor、schema、错误 envelope。 | CLI 可 inspect MCP surface。 |
| 2 | 实现 mock/client trait 和 allowlist 检查。 | 未授权 tool 被拒绝。 |
| 3 | 实现 tool mapping 和 policy helper。 | Adapter MCP transport 可调用。 |
| 4 | 实现最小 server surface。 | 外部 client 只看到受控工具。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Client | tool/resource/prompt 调用 | 已完成 V1.8.3 | 后续补真实 streaming 数据面、auth 和兼容性矩阵。 |
| Server | Eva tools 暴露 | 未实现 | 先 `agent.invoke`、`adapter.list`。 |
| Mapping | MCP 到 capability | 未实现 | 处理 schema mismatch。 |
| Policy | allowlist 和 scope | 未实现 | 输出 request gate 输入。 |
| Schema | 输入输出和版本 | 未实现 | 定义稳定 envelope。 |
