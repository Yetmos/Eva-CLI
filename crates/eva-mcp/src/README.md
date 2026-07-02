# eva-mcp/src / MCP 源码

![V1.x extension module flow](../../assets/eva-extension-module-flow.svg)

本目录承载 MCP client/server、tool mapping、policy helper 和 schema 边界。当前为骨架，V1.1 先实现受 allowlist 限制的 client/mapping 和受控 server surface。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 骨架 | V1.1 |
| `client.rs` | MCP client protocol integration | 骨架 | V1.1 |
| `server.rs` | 受控 MCP server exposure | 骨架 | V1.1 |
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
| Client | tool/resource/prompt 调用 | 未实现 | 定义 trait 和 timeout。 |
| Server | Eva tools 暴露 | 未实现 | 先 `agent.invoke`、`adapter.list`。 |
| Mapping | MCP 到 capability | 未实现 | 处理 schema mismatch。 |
| Policy | allowlist 和 scope | 未实现 | 输出 request gate 输入。 |
| Schema | 输入输出和版本 | 未实现 | 定义稳定 envelope。 |
