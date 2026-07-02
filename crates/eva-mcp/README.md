# eva-mcp / MCP 协议边界

更新时间：2026-07-02

![V1.x extension module flow](../assets/eva-extension-module-flow.svg)

`eva-mcp` 负责 Model Context Protocol 的 client/server 协议边界、tool/resource/prompt 映射、MCP policy helper 和 schema 约束。它不把内部 Topic 无限制代理给外部 MCP client，也不让外部 MCP server 绕过 AdapterRuntime 和 policy。

## 当前模块功能说明

| 功能域 | 当前状态 | 目标行为 |
| --- | --- | --- |
| Client | 骨架 | 连接已配置 MCP server，执行受 allowlist 限制的 tool/resource/prompt。 |
| Server | 骨架 | 对外暴露 Eva 的受控工具，例如 agent invoke、adapter list。 |
| Tool mapping | 骨架 | 将 MCP tool/resource/prompt 转成 Eva capability 或 adapter invocation。 |
| Policy helper | 骨架 | 根据 client/server、tool name、schema、scope 生成 policy 检查输入。 |
| Schema | 骨架 | 定义 MCP 输入输出 schema、错误 envelope 和版本兼容边界。 |
| Discovery 接入 | 未实现 | V1.1 由 `eva-discovery` 扫描候选，授权仍在 Adapter/MCP policy gate。 |

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
| 7 | V1.5 | 增加兼容性测试、schema 版本迁移、流式响应边界。 | 协议稳定后 | 不同 MCP server 版本有明确错误。 |

## 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 模块导出 | 骨架 | re-export client、server、mapping、policy、schema。 |
| `src/client.rs` | MCP client integration | `RESPONSIBILITY` 占位 | 定义 client trait、request/response、timeout。 |
| `src/server.rs` | 受控 MCP server | `RESPONSIBILITY` 占位 | 定义 server tool surface 和 authorization hook。 |
| `src/tool_mapping.rs` | tool/resource/prompt 映射 | `RESPONSIBILITY` 占位 | 定义 mapping table 和冲突处理。 |
| `src/policy.rs` | MCP policy helper | `RESPONSIBILITY` 占位 | 定义 allowlist、scope、request gate 输入。 |
| `src/schema.rs` | MCP schema 边界 | `RESPONSIBILITY` 占位 | 定义输入输出 schema 和错误 envelope。 |
| `src/README.md` | 源码目录说明 | 简略 | 补充文件职责和进度。 |
| 单元测试 | mapping/policy/schema | 未开始 | 覆盖未授权 tool、schema mismatch、server surface。 |

## 验证计划

| 阶段 | 命令 | 目标 |
| --- | --- | --- |
| V1.1 | `cargo test -p eva-mcp` | schema、mapping、policy helper 可测。 |
| V1.1 | `cargo test -p eva-adapter` | MCP transport 只调用 allowlist tool。 |
| V1.5 | MCP compatibility tests | server/client 版本差异有稳定错误。 |

## English

`eva-mcp` owns MCP protocol boundaries, client/server surfaces, tool mapping, policy helpers, and schemas. It must not expose unlimited internal Topic or runtime state proxies.
