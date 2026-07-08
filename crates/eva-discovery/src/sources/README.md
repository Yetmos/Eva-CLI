# Discovery Sources / 发现来源

![V1.x extension module flow](../../../assets/eva-extension-module-flow.svg)

本目录承载 discovery 的 trusted source adapter。每个来源只输出候选和拒绝原因，不能返回 executable handle，也不能执行候选能力。

## 功能说明

| 文件 | 来源 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `mod.rs` | source 模块导出和通用边界 | 已实现 | V1.9.3 |
| `project_agents.rs` | 项目 Agent manifests | 保留占位 | 后续 |
| `project_adapters.rs` | 项目 Adapter manifests | 保留占位 | 后续 |
| `path_commands.rs` | 配置允许的本地命令名 | 已实现 | V1.9.3 |
| `mcp.rs` | 已配置 MCP server allowlist surface | 已实现 | V1.9.3 |
| `omx.rs` | 受信 OMX workflow surface | 已实现 | V1.9.3 |
| `codex.rs` | 受信 Codex capability surface | 已实现 | V1.9.3 |
| `registry.rs` | 外部 registry 配置边界 | 已实现边界 | V1.9.3 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义统一 `DiscoverySource` trait。 | Scanner 可统一调用。 |
| 2 | 通过 `ProjectDiscoverySource` 实现 project manifest source。 | 项目内候选可发现。 |
| 3 | 实现 path/MCP/workflow/registry sources。 | 外部候选可发现但不可执行。 |
| 4 | 为每个 source 增加 timeout、cache key、reject reason 和 health hints。 | CLI 可解释为什么不可用。 |

## 进度表

| Source | 风险点 | 状态 | 下一步 |
| --- | --- | --- | --- |
| project config | manifest 不一致 | 已实现 | 继续复用 `ProjectConfig` typed loader。 |
| path commands | 任意命令发现 | 已实现 | 只记录 manifest 命令名，不执行 PATH lookup。 |
| mcp | tool 过度暴露 | 已实现 | 只记录已配置 allowlist。 |
| omx/codex | workflow 权限混淆 | 已实现 | 只输出 trusted surface，不授予 handle。 |
| external registry | 真实 registry 协议和认证 | 边界已实现 | 后续接 registry auth、协议和缓存。 |
