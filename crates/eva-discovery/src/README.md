# eva-discovery/src / 发现源码

![V1.x extension module flow](../../assets/eva-extension-module-flow.svg)

## V1.1 Implemented Surface

- `normalizer.rs`: `DiscoveryCandidate`, candidate kind, trust level, rejected reason, and dedupe rules.
- `scanner.rs`: `DiscoverySource`, source reports, `scan_sources`, and `ProjectDiscoverySource` over `ProjectConfig`.
- `service.rs`: `DiscoveryService::scan_project`, cache access, candidate access, and health projection.
- `cache.rs`: in-memory snapshot replacement with refresh reason.
- `health.rs`: side-effect-free `seen`/`rejected` health status.

The important invariant is explicit: every V1.1 candidate has `handle_granted=false`. Adapter execution authority is issued only by `eva-adapter::AdapterRuntime` after configuration and routing checks.

本目录承载 discovery service、scanner、normalizer、health、cache 和 trusted sources。当前为骨架，V1.1 先建立“候选发现不等于授权执行”的数据链路。

## 功能说明

| 文件/目录 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 骨架 | V1.1 |
| `service.rs` | Discovery 协调服务 | 骨架 | V1.1 |
| `scanner.rs` | source 调度和错误聚合 | 骨架 | V1.1 |
| `normalizer.rs` | candidate 归一化和 reject reason | 骨架 | V1.1 |
| `health.rs` | 无副作用健康探测 | 骨架 | V1.1 |
| `cache.rs` | discovery 快照和 TTL | 骨架 | V1.1 |
| `sources/` | trusted source adapters | 骨架 | V1.1 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义 DiscoveryCandidate、reject reason、health status。 | CLI 可显示候选。 |
| 2 | 定义 source trait 和 scanner orchestration。 | 多来源扫描可组合。 |
| 3 | 实现 normalizer、cache、health。 | 候选稳定、可审计。 |
| 4 | 分批实现 project、path、mcp、omx、codex sources。 | 只扫描受信来源。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Service | scan/refresh/cache API | 未实现 | 定义公共入口。 |
| Scanner | source 调度 | 未实现 | 处理 source 失败隔离。 |
| Normalizer | candidate/reject reason | 未实现 | 定义 dedupe 规则。 |
| Health | 轻量 probe | 未实现 | 禁止副作用 probe。 |
| Cache | snapshot/TTL | 未实现 | 设计 refresh 语义。 |
