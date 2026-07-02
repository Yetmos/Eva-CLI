# Adapter Transports / Adapter 传输实现

![V1.x extension module flow](../../../assets/eva-extension-module-flow.svg)

本目录承载 Adapter 的具体 transport 实现。所有 transport 都必须由 manifest、effective policy、schema 和 audit gate 约束，禁止变成任意插件或任意 shell/HTTP 代理。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `mod.rs` | transport 模块导出和通用边界 | 骨架 | V1.1 |
| `builtin.rs` | 内置本地能力 transport | 骨架 | V1.1 |
| `stdio.rs` | 受控 stdio command transport | 骨架 | V1.1 |
| `http.rs` | 受控 HTTP transport | 骨架 | V1.1 |
| `mcp.rs` | MCP tool/resource/prompt transport | 骨架 | V1.1 |
| `skill.rs` | workflow skill transport | 骨架 | V1.1 |
| `eventbus.rs` | EventBus-backed transport | 骨架 | V1.1 |
| `lua_capability.rs` | Lua capability bridge | 骨架 | V1.1 |
| `hardware.rs` | hardware device transport | 骨架 | V1.3 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义统一 `Transport` trait 和 invocation/result envelope。 | Runtime 可统一调用。 |
| 2 | 实现 builtin transport，用于无副作用回归。 | 基础集成测试可运行。 |
| 3 | 实现 stdio/http，并强制 command/URL allowlist。 | 外部调用受 manifest 限制。 |
| 4 | 实现 MCP/skill/hardware 桥接。 | 扩展能力进入同一审计路径。 |

## 进度表

| Transport | 风险点 | 状态 | 下一步 |
| --- | --- | --- | --- |
| builtin | 低风险，本地无副作用 | 未实现 | 先完成 demo provider。 |
| stdio | shell 注入、参数拼接 | 未实现 | command 和 args 必须分离。 |
| http | 任意 URL、凭据泄露 | 未实现 | URL/env allowlist。 |
| mcp | tool 滥用 | 未实现 | tool/resource/prompt allowlist。 |
| skill | workflow 越权 | 未实现 | 固定 skill entrypoint。 |
| hardware | raw I/O | 未实现 | 只接 DeviceRegistry handle。 |
