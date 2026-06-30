# Eva-CLI Configuration / Eva-CLI 配置

## 中文

本目录按照 `docs/项目配置方案.md` 建立 Eva-CLI 的项目配置根。配置文件使用 YAML 作为人工维护格式，JSON Schema 作为校验格式，运行时协议消息继续使用 JSON。

目录职责：

| 路径 | 说明 |
| --- | --- |
| `eva.yaml` | 全局运行时、EventBus、Scheduler、状态、记忆、知识库和观测配置。 |
| `agents/` | Lua Agent 的声明、约束和入口脚本。 |
| `adapters/` | Codex CLI、Claude API、MCP、Skill、硬件等 Adapter manifest。 |
| `capabilities/` | Skill、MCP、Tool 等能力定义。 |
| `policies/` | 沙箱、Adapter、MCP、硬件访问策略。 |
| `routes/` | Topic 路由表。 |
| `schemas/` | 配置 JSON Schema。 |

安全规则：本目录只保存环境变量名、权限边界和连接声明，不保存 API key、token 或用户密钥明文。

## English

This directory follows `docs/en/project-configuration.md` and contains the Eva-CLI project configuration root. YAML is used for human-maintained configuration, JSON Schema is used for validation, and runtime protocol messages remain JSON.

Directory ownership:

| Path | Purpose |
| --- | --- |
| `eva.yaml` | Global runtime, EventBus, Scheduler, state, memory, knowledge, and observability settings. |
| `agents/` | Lua Agent manifests, constraints, and entry scripts. |
| `adapters/` | Adapter manifests for Codex CLI, Claude API, MCP, workflow skills, and hardware. |
| `capabilities/` | Capability definitions for Skills, MCP, and Tools. |
| `policies/` | Sandbox, Adapter, MCP, and hardware access policies. |
| `routes/` | Topic routing table. |
| `schemas/` | JSON Schemas for configuration validation. |

Security rule: store environment variable names, permissions, and connection declarations here, never plaintext API keys, tokens, or user secrets.
