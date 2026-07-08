# eva-discovery/src / 发现源码

![V1.x extension module flow](../../assets/eva-extension-module-flow.svg)

## V1.9.3 Implemented Surface

- `normalizer.rs`: `DiscoveryCandidate`, candidate kind, trust level, rejected reason, and dedupe rules.
- `scanner.rs`: `DiscoverySource`, timeout/cache key hooks, source reports, `scan_sources`, and `ProjectDiscoverySource` over `ProjectConfig`.
- `service.rs`: `DiscoveryService::scan_project`, `scan_project_incremental`, cache access, candidate access, and health projection.
- `cache.rs`: in-memory snapshot replacement, source merge, and refresh reason.
- `health.rs`: side-effect-free `seen`/`rejected` health status.
- `sources/`: PATH command, MCP, OMX, Codex, and external registry source boundaries.

The important invariant is explicit: every V1.9.3 candidate has `handle_granted=false`. Adapter execution authority is issued only by `eva-adapter::AdapterRuntime` after configuration, routing, and policy checks.

本目录承载 discovery service、scanner、normalizer、health、cache 和 trusted sources。当前已具备 V1.9.3 多来源、source report 和增量缓存基线；后续继续补真实 registry 协议、source auth、TTL/持久 cache 和生产健康探测。

## 功能说明

| 文件/目录 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 已实现 | V1.9.3 |
| `service.rs` | Discovery 协调服务 | 已实现 | V1.9.3 |
| `scanner.rs` | source 调度和错误聚合 | 已实现 | V1.9.3 |
| `normalizer.rs` | candidate 归一化和 reject reason | 已实现 | V1.9.3 |
| `health.rs` | 无副作用健康探测 | 已实现基础 | V1.9.3 |
| `cache.rs` | discovery 快照和 source merge | 已实现基础 | V1.9.3 |
| `sources/` | trusted source adapters | 已实现多来源基线 | V1.9.3 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义 DiscoveryCandidate、reject reason、health status。 | CLI 可显示候选。 |
| 2 | 定义 source trait 和 scanner orchestration。 | 多来源扫描可组合。 |
| 3 | 实现 normalizer、cache、health。 | 候选稳定、可审计。 |
| 4 | 分批实现 project、path、mcp、omx、codex、registry sources。 | 只扫描受信来源。 |
| 5 | 增加 source timeout、cache key、reject reason 和增量缓存。 | 慢 source 不阻塞整体扫描。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Service | scan/refresh/cache API | 已实现 | 接跨进程 cache 和真实健康探测。 |
| Scanner | source 调度 | 已实现 | 接 async timeout、metrics 和更细 source auth。 |
| Normalizer | candidate/reject reason | 已实现 | 补 schema mismatch 明细。 |
| Health | 轻量 probe | 已实现基础 | 禁止副作用 probe，后续接 provider health metadata。 |
| Cache | snapshot/source merge | 已实现基础 | 设计 TTL、过期和持久化语义。 |
