# eva-discovery / 能力发现

更新时间：2026-07-02

![V1.x extension module flow](../assets/eva-extension-module-flow.svg)

`eva-discovery` 负责受信来源扫描、候选归一化、健康探测和缓存。它的核心原则是“发现不等于授权”：Discovery 只返回 candidate、health 和 rejected reason，不能授予执行 handle，不能绕过 manifest、policy 或 AdapterRuntime。

## 当前模块功能说明

| 功能域 | 当前状态 | 目标行为 |
| --- | --- | --- |
| Discovery service | 骨架 | 协调来源扫描、健康探测、归一化和缓存。 |
| Scanner | 骨架 | 调用各 trusted source adapter，收集候选。 |
| Normalizer | 骨架 | 统一 candidate ID、capability、provider、trust reason、reject reason。 |
| Health | 骨架 | 对候选做轻量 probe，不产生副作用。 |
| Cache | 骨架 | 缓存 discovery 结果，避免每次启动重复扫描。 |
| Project agent source | 骨架 | 从项目 Agent manifest 发现本地 Agent 能力。 |
| Project adapter source | 骨架 | 从 Adapter manifest 发现外部 provider 候选。 |
| PATH command source | 骨架 | 只扫描配置允许的命令路径。 |
| MCP source | 骨架 | 发现已配置 MCP server 的 tool/resource/prompt。 |
| OMX/Codex source | 骨架 | 发现受信 workflow 或 Codex 能力 surface。 |

## 模块边界

`eva-discovery` 做：

- 扫描受信来源并输出候选列表。
- 标准化候选字段和拒绝原因。
- 做无副作用健康探测。
- 缓存 discovery 快照并记录 trace/audit。

`eva-discovery` 不做：

- 不授予执行权限。
- 不保存 AdapterRuntime handle。
- 不执行 shell、HTTP、MCP tool 或硬件 I/O。
- 不把未配置来源纳入候选。

## 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V1.1 | 定义 `DiscoveryCandidate`、source id、trust level、reject reason、health status。 | `eva-core` | Candidate 可用于 CLI JSON 输出。 |
| 2 | V1.1 | 实现 source trait 和 scanner orchestration。 | 标准库 | 单个 source 失败不影响其他 source。 |
| 3 | V1.1 | 实现 project_agents/project_adapters sources。 | `eva-config` | manifest 候选可发现但不可执行。 |
| 4 | V1.1 | 实现 path_commands、mcp、omx、codex sources 的 allowlist 边界。 | 配置和 policy | 未信任路径不扫描。 |
| 5 | V1.1 | 实现 normalizer 和 rejected reason 聚合。 | `eva-observability` | 重复候选和 schema mismatch 有清晰原因。 |
| 6 | V1.1 | 实现 cache 和 health probe 初版。 | storage 可选 | 缓存过期和手动 refresh 可测。 |
| 7 | V1.5 | 增加 discovery 指标、慢 source 超时、增量扫描。 | runtime observability | 启动时 discovery 可控且可审计。 |

## 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 模块导出 | 骨架 | re-export service、scanner、normalizer、health、cache、sources。 |
| `src/service.rs` | Discovery 协调服务 | `RESPONSIBILITY` 占位 | 定义 scan/refresh/cache API。 |
| `src/scanner.rs` | source 调度 | `RESPONSIBILITY` 占位 | 定义 source trait、超时、错误聚合。 |
| `src/normalizer.rs` | 候选归一化 | `RESPONSIBILITY` 占位 | 定义 candidate、reject reason、dedupe。 |
| `src/health.rs` | 健康探测 | `RESPONSIBILITY` 占位 | 定义无副作用 probe 结果。 |
| `src/cache.rs` | 发现缓存 | `RESPONSIBILITY` 占位 | 定义 snapshot、TTL、refresh reason。 |
| `src/sources/project_agents.rs` | 项目 Agent manifest 来源 | 骨架 | 从 ProjectConfig 提取 Agent 候选。 |
| `src/sources/project_adapters.rs` | 项目 Adapter manifest 来源 | 骨架 | 从 AdapterManifest 提取 provider 候选。 |
| `src/sources/path_commands.rs` | PATH 命令来源 | 骨架 | 只扫描 allowlist 路径和命令。 |
| `src/sources/mcp.rs` | MCP 来源 | 骨架 | 从配置的 MCP endpoint 提取 tool 候选。 |
| `src/sources/omx.rs` | OMX workflow 来源 | 骨架 | 只暴露受信 workflow surface。 |
| `src/sources/codex.rs` | Codex 能力来源 | 骨架 | 只记录候选，不授予调用。 |
| `src/README.md` | 源码目录说明 | 简略 | 补充文件职责和进度。 |
| `src/sources/README.md` | source 目录说明 | 简略 | 补充各来源信任边界。 |

## 验证计划

| 阶段 | 命令 | 目标 |
| --- | --- | --- |
| V1.1 | `cargo test -p eva-discovery` | source、normalizer、cache、health 可测。 |
| V1.1 | Adapter/MCP integration tests | discovery candidate 不等于 executable handle。 |
| V1.5 | startup performance tests | 慢 source 不拖垮启动。 |

## English

`eva-discovery` owns trusted source scanning, normalization, health probing, and cache. Discovery returns candidates and rejected reasons only; authorization remains outside this crate.
