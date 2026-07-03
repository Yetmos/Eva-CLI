# Examples

更新时间：2026-07-03

本目录维护 Eva-CLI 的可运行示例、配置样例和集成演示。

| 示例 | 状态 | 说明 |
| --- | --- | --- |
| `basic/` | V0.5 已实现 | 最小可运行事件闭环 + task status/logs/cancel + timeout/dead-letter replay 诊断。 |
| `agent/` | 计划中 | 更复杂的 Lua Agent 示例。 |
| `adapter/` | 计划中 | 外部 Adapter 示例。 |
| `mcp/` | 计划中 | MCP 集成示例。 |
| `hardware/` | 计划中 | 硬件接入示例。 |

运行当前示例：

```powershell
cargo run -- run --example basic --output json
cargo run -- task status --output json
```
