# Discovery Sources / 发现来源

![V1.x extension module flow](../../../assets/eva-extension-module-flow.svg)

本目录承载 discovery 的 trusted source adapter。每个来源只输出候选和拒绝原因，不能返回 executable handle，也不能执行候选能力。

## 功能说明

| 文件 | 来源 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `mod.rs` | source 模块导出和通用边界 | 骨架 | V1.1 |
| `project_agents.rs` | 项目 Agent manifests | 骨架 | V1.1 |
| `project_adapters.rs` | 项目 Adapter manifests | 骨架 | V1.1 |
| `path_commands.rs` | 配置允许的本地命令路径 | 骨架 | V1.1 |
| `mcp.rs` | 已配置 MCP server surface | 骨架 | V1.1 |
| `omx.rs` | 受信 OMX workflow surface | 骨架 | V1.1 |
| `codex.rs` | 受信 Codex capability surface | 骨架 | V1.1 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义统一 `DiscoverySource` trait。 | Scanner 可统一调用。 |
| 2 | 实现 project manifest sources。 | 项目内候选可发现。 |
| 3 | 实现 path/MCP/workflow sources。 | 外部候选可发现但不可执行。 |
| 4 | 为每个 source 增加 reject reason 和 health hints。 | CLI 可解释为什么不可用。 |

## 进度表

| Source | 风险点 | 状态 | 下一步 |
| --- | --- | --- | --- |
| project agents | manifest 不一致 | 未实现 | 复用 `ProjectConfig`。 |
| project adapters | provider 重复 | 未实现 | 输出候选冲突。 |
| path commands | 任意命令发现 | 未实现 | 只扫 allowlist。 |
| mcp | tool 过度暴露 | 未实现 | 只记录已配置 server。 |
| omx/codex | workflow 权限混淆 | 未实现 | 只输出 trusted surface。 |
