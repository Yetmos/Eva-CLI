# eva-observability / 可观测性

更新时间：2026-07-08

![Eva module implementation roadmap](../assets/eva-module-implementation-roadmap.svg)

`eva-observability` 定义 Eva-CLI 运行时、CLI、Adapter、Agent 和服务模块共享的 trace、audit 与 metrics 契约。它保存稳定字段、枚举、sink trait，并在 V1.9.5 提供 best-effort file JSONL backend 基线，用于写入 audit、metrics 和 OpenTelemetry-style span export。它不做业务路由，也不做权限判断。V1.6.3 的 filesystem durable audit sink 位于 `eva-storage`，以避免本 crate 反向依赖存储层。

## 中文

### 当前实现状态

| 范围 | 状态 | 说明 |
| --- | --- | --- |
| `TraceFields` | 已完成 V1.9.5 | 聚合 event、request、topic、agent、adapter、capability、provider、correlation、causation、generation、span；支持 provider invocation trace builder、child span 和 continuity key |
| `SpanId` | 已完成 | 稳定 span identifier，限制为跨平台 ASCII 字符 |
| `TraceFields::from_event` | 已完成 | 从 `eva-core::Event` 提取不含 payload 的链路字段 |
| `AuditAction` | 已完成 V1.10.2 | 定义配置、policy、runtime、event、Lua host log/audit、capability、adapter、MCP session/stream/proxy、hardware driver/hotplug、安全拒绝等稳定动作 |
| `AuditOutcome` | 已完成 | `ok`、`planned`、`blocked`、`failed` |
| `AuditEvent` | 已完成 | 记录动作、结果、trace、消息、扩展字段和时间 |
| `AuditSink` | 已完成 | 抽象审计写入 trait |
| `InMemoryAuditSink` | 已完成 | 测试和 dry-run 可用的内存 sink |
| `MetricName` | 已完成 | 稳定 metric 名称校验 |
| `MetricLabels` | 已完成 V1.9.5 | 使用 `BTreeMap` 保证标签顺序稳定；提供 runtime/provider/task 标签 helper |
| `MetricPoint` | 已完成 | 表示一个 counter/gauge/histogram 数据点 |
| `FileObservabilitySink` | 已完成 V1.9.5 | 写入 `audit.jsonl`、`metrics.jsonl` 和 `otel-spans.jsonl`，不引入外部 SDK 依赖 |
| `BestEffortObservabilityPipeline` | 已完成 V1.9.5 | 后端不可用或写入失败时降级到 in-memory audit/metrics，并记录 degraded reason |
| 具体后端 | 部分实现 V1.9.5 | `eva-storage::FileSystemAuditSink` 可写 durable backend `audit/`；本 crate 提供 JSONL 后端和 OTel-style span export；真实 tracing subscriber、OpenTelemetry SDK exporter、db sink、retention/rotation 和完整 runtime wiring 仍是后续范围 |

### 公开 API

| API | 输入 | 输出 | 用途 |
| --- | --- | --- | --- |
| `TraceFields::from_event` | `&eva_core::Event` | `TraceFields` | 从核心事件构造观测字段 |
| `TraceFields::with_request_id` | `RequestId` | `TraceFields` | 为 CLI/Adapter 调用补充 request-level trace |
| `TraceFields::child_span` | `SpanId` | `TraceFields` | 继承 request/correlation 等字段并替换 span |
| `TraceFields::continuity_key` | `&self` | `String` | 生成跨 CLI/runtime/provider/Lua 的稳定链路 key |
| `TraceFields::entries` | `&self` | `Vec<(&'static str, String)>` | 输出当前存在的字段 |
| `SpanId::parse` | `&str` | `Result<SpanId, EvaError>` | 校验 span id |
| `AuditEvent::new` | action、outcome、trace | `AuditEvent` | 构造审计记录 |
| `AuditEvent::with_message` | `impl Into<String>` | `AuditEvent` | 添加人类可读摘要 |
| `AuditEvent::with_field` | key/value | `AuditEvent` | 添加结构化扩展字段 |
| `AuditSink::record` | `AuditEvent` | `Result<(), EvaError>` | 审计写入抽象 |
| `MetricName::parse` | `&str` | `Result<MetricName, EvaError>` | 校验 metric 名称 |
| `MetricLabels::with` | key/value | `MetricLabels` | 添加稳定标签 |
| `MetricLabels::runtime/provider/task` | 运行面字段 | `MetricLabels` | 生成 runtime、provider 和 task 指标标签 |
| `MetricPoint::new` | name、kind、value | `MetricPoint` | 构造指标点 |
| `FileObservabilitySink::open` | backend root | `Result<FileObservabilitySink, EvaError>` | 打开 audit/metrics/span JSONL 后端 |
| `BestEffortObservabilityPipeline::open` | backend root | `BestEffortObservabilityPipeline` | 打开可降级观测 pipeline |
| `BestEffortObservabilityPipeline::export_span` | span 名称、trace、attributes | `Result<(), EvaError>` | 输出 OTel-style span JSONL |

### 稳定字段

`TraceFields` 的字段名应作为 CLI JSON、audit、metrics 标签和调试输出的统一来源：

```text
event_id
request_id
topic
agent_id
adapter_id
capability
provider
correlation_id
causation_id
generation_id
span_id
```

Provider invocation records should carry the same field names in their local
data payload as the top-level CLI envelope uses for command-level trace.

### 边界

`eva-observability` 做：

- 定义稳定观测字段。
- 定义 audit action/outcome 枚举。
- 定义 sink trait。
- 提供内存 sink 方便测试。
- 提供 V1.9.5 file JSONL backend 和 best-effort pipeline 基线。

`eva-observability` 不做：

- 不启动 tracing subscriber。
- 不在默认路径隐式写文件；只有显式使用 `FileObservabilitySink` 或 `BestEffortObservabilityPipeline` 时写 JSONL。
- 不调用 OpenTelemetry SDK；V1.9.5 只输出 OTel-style JSONL span adapter。
- 不提供数据库 sink、retention/rotation 或生产 metrics exporter。
- 不根据观测结果做 policy 或 routing 决策。
- 不解释 event payload。

### 测试与验证

| 命令 | 当前结果 |
| --- | --- |
| `cargo test -p eva-observability` | 通过 |
| `cargo test --workspace` | 通过 |

关键测试覆盖：

| 测试 | 覆盖内容 |
| --- | --- |
| `trace_fields_extract_core_event_metadata` | 从 `Event` 提取 trace 字段 |
| `entries_include_only_present_values` | 只输出存在的 trace 字段 |
| `audit_action_spelling_is_stable` | action/outcome 字符串稳定 |
| `in_memory_sink_records_events` | audit sink 行为 |
| `metric_name_rejects_unstable_values` | metric 名称校验 |
| `labels_are_deterministically_ordered` | metric labels 顺序稳定 |
| `file_observability_sink_writes_audit_metrics_and_otel_span` | JSONL backend 写入 audit、metrics 和 OTel-style span |
| `best_effort_pipeline_degrades_without_failing` | 后端不可用时降级且不阻塞调用方 |
| `metric_labels_cover_runtime_provider_and_task_surfaces` | runtime/provider/task 标签 helper |
| `child_span_preserves_trace_continuity` | child span 和 continuity key 语义 |
| `audit_action_spelling_is_stable` | 覆盖 hardware driver/hotplug audit action 稳定拼写 |

### 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V0.2 | 定义 `TraceFields` 和 `SpanId`，支持从核心事件提取链路字段。 | `eva-core` | 字段名稳定且不包含 payload。 |
| 2 | V0.2 | 定义 `AuditAction`、`AuditOutcome`、`AuditEvent` 和 sink trait。 | `eva-core::EvaError` | audit action 字符串稳定。 |
| 3 | V0.2 | 定义 `MetricName`、`MetricLabels`、`MetricPoint`。 | 标准库 `BTreeMap` | 标签顺序稳定。 |
| 4 | V0.3 | 接 CLI 输出 envelope 和诊断 trace 字段。 | `eva-cli` | human/json 输出共享同一 trace 字段。 |
| 5 | V0.4 | 接 runtime、eventbus、scheduler、agent 的 audit/metrics。 | runtime 主链路 | 事件闭环每个阶段有 trace。 |
| 6 | V1.1+ | 接 adapter、MCP、discovery、hardware、backup、lifecycle 审计动作。 | 扩展模块 | 外部能力和高风险操作可审计；V1.8.3 已加入 MCP session/stream/proxy 动作。 |
| 7 | V1.9.5 | 接 file JSONL backend、OTel-style span export 和 best-effort pipeline。 | 发布阶段选型 | 后端不改变公共字段契约，后端不可用时不阻塞核心任务。 |
| 8 | 后续 | 接真实 tracing subscriber、OpenTelemetry SDK exporter、db sink、retention/rotation 和 runtime wiring。 | 常驻 runtime | 生产 runtime 事件、指标和 span 全量接入观测后端。 |

### 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 公共导出 | 已完成 | 随新观测动作扩展 re-export。 |
| `src/trace.rs` | trace 字段、span id、event 提取、child span、continuity key | 已完成 V1.9.5 | V0.4 接 runtime/eventbus/agent span。 |
| `src/audit.rs` | audit action/outcome/event/sink | 已完成 V1.10.2 | 后续继续追加高风险 apply 和分发动作。 |
| `src/metrics.rs` | metric name、labels、point、runtime/provider/task labels | 已完成 V1.9.5 | V0.4 定义 runtime/eventbus 指标命名。 |
| `src/backend.rs` | file JSONL backend、OTel-style span export、best-effort degradation、smoke report | 已完成 V1.9.5 | 接真实 OpenTelemetry SDK exporter、db sink、retention/rotation 和 runtime wiring。 |
| `src/README.md` | 源码目录说明 | 已更新 V1.9.5 | 随后端与 runtime wiring 继续同步。 |
| concrete backend | tracing/OpenTelemetry SDK/db/rotation | 部分实现 V1.9.5 | 后续独立选型并接入 runtime。 |

## English

`eva-observability` defines shared trace, audit, and metrics contracts. V1.9.5 adds a best-effort file JSONL backend, OTel-style span JSONL export, runtime/provider/task label helpers, and trace continuity helpers. It does not install a tracing subscriber, call the OpenTelemetry SDK, route business logic, or authorize requests.
