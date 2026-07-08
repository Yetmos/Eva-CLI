# eva-discovery / 能力发现

更新时间：2026-07-08

![V1.x extension module flow](../assets/eva-extension-module-flow.svg)

`eva-discovery` 负责受信来源扫描、候选归一化、健康探测和缓存。它的核心原则是“发现不等于授权”：Discovery 只返回 candidate、health 和 rejected reason，不能授予执行 handle，不能绕过 manifest、policy 或 AdapterRuntime。

## 当前模块功能说明

| 功能域 | 当前状态 | 目标行为 |
| --- | --- | --- |
| Discovery service | 已实现 V1.9.3 基线 | 协调多来源扫描、健康投影、归一化和缓存。 |
| Scanner | 已实现 | 调用 trusted source adapter，记录 source timeout、cache key、elapsed/status 和 reject reason。 |
| Normalizer | 已实现 | 统一 candidate ID、capability、provider、trust reason、reject reason，所有候选不授予 handle。 |
| Health | 已实现 | 从候选生成无副作用 `seen/rejected` 健康状态。 |
| Cache | 已实现基础 | 支持全量 replace 和按 source 增量 merge；TTL/跨进程 cache 后续实现。 |
| Project config source | 已实现 | 从项目 Adapter/Capability 配置发现受信候选。 |
| PATH command source | 已实现 | 只记录 stdio Adapter manifest 中配置的命令名，不执行 PATH lookup。 |
| MCP source | 已实现 | 从 MCP Adapter allowlist 发现 tool 候选。 |
| OMX/Codex source | 已实现 | 发现受信 workflow/Codex surface，未配置状态输出 rejected reason。 |
| External registry source | 已实现边界 | 记录 registry source 是否配置；真实 registry 协议和认证后续实现。 |

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
| `src/lib.rs` | 模块导出 | 已实现 | 继续按公共 API 稳定性收敛导出。 |
| `src/service.rs` | Discovery 协调服务 | 已实现 | 后续接跨进程 cache 和生产健康探测。 |
| `src/scanner.rs` | source 调度 | 已实现 | 后续接真实 async timeout/metrics。 |
| `src/normalizer.rs` | 候选归一化 | 已实现 | 后续补 source-specific schema mismatch 明细。 |
| `src/health.rs` | 健康探测 | 已实现基础 | 后续接无副作用 provider health probe。 |
| `src/cache.rs` | 发现缓存 | 已实现基础 | 后续实现 TTL、过期和持久化。 |
| `src/sources/path_commands.rs` | PATH 命令来源 | 已实现 | 后续接 source auth 和路径策略。 |
| `src/sources/mcp.rs` | MCP 来源 | 已实现 | 后续接真实 MCP registry/protocol metadata。 |
| `src/sources/omx.rs` | OMX workflow 来源 | 已实现 | 后续接更细 workflow trust metadata。 |
| `src/sources/codex.rs` | Codex 能力来源 | 已实现 | 后续接 Codex source auth 和版本 metadata。 |
| `src/sources/registry.rs` | 外部 registry 来源 | 已实现边界 | 后续接真实 registry 协议、认证和缓存。 |
| `src/README.md` | 源码目录说明 | 已更新 | 随 V1.9.x 继续维护。 |
| `src/sources/README.md` | source 目录说明 | 简略 | 补充各来源信任边界。 |

## 验证计划

| 阶段 | 命令 | 目标 |
| --- | --- | --- |
| V1.1 | `cargo test -p eva-discovery` | source、normalizer、cache、health 可测。 |
| V1.1 | Adapter/MCP integration tests | discovery candidate 不等于 executable handle。 |
| V1.5 | startup performance tests | 慢 source 不拖垮启动。 |

## V1.9.3 Status

V1.9.3 implements multi-source discovery while preserving the central boundary: discovery never grants executable handles.

- `DiscoveryCandidate` records candidate id, kind, source, trust level, optional adapter id, optional capability, rejected reason, and `handle_granted=false`.
- `scan_sources` isolates source timeout/error/rejection and emits `DiscoverySourceReport` with source id, cache key, timeout, elapsed, status, error, and rejected reason.
- `DiscoveryService::scan_project` performs a full multi-source scan; `scan_project_incremental` merges successful source snapshots into `DiscoveryCache`.
- PATH command, MCP, OMX, Codex, project config, and external registry sources are now visible as isolated reports.
- `DiscoveryHealth` reports `seen` or `rejected` status from candidates without side effects.

Runtime execution must still go through `eva-adapter::AdapterRuntime`, provider routing, and policy gates.

## V1.9.3 Verification

```powershell
cargo test -p eva-discovery
cargo test -p eva-cli discovery_scan_json_reports_source_statuses
cargo run -- discovery scan --output json
```
