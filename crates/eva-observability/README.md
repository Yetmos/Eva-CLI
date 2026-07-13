# eva-observability / 可观测性

更新时间：2026-07-10

![Eva module implementation roadmap](../assets/eva-module-implementation-roadmap.svg)

`eva-observability` 定义 Eva-CLI 运行时、CLI、Adapter、Agent 和服务模块共享的 trace、audit 与 metrics 契约。它保存稳定字段、枚举、sink trait，并在 V1.9.5 提供 best-effort file JSONL backend 基线，用于写入 audit、metrics 和 OpenTelemetry-style span export；V1.16.3 追加基于 OpenTelemetry SDK 的 OTLP HTTP/protobuf trace/metrics exporter smoke；V1.16.4 增加显式 retention/rotation/corrupt-record policy，用于 JSONL 文件 sink 和 durable audit sink。它不做业务路由，也不做权限判断。V1.6.3 的 filesystem durable audit sink 位于 `eva-storage`，以避免本 crate 反向依赖存储层。

## 中文

### 当前实现状态

| 范围 | 状态 | 说明 |
| --- | --- | --- |
| `TraceFields` | 已完成 V1.9.5 | 聚合 event、request、topic、agent、adapter、capability、provider、correlation、causation、generation、span；支持 provider invocation trace builder、child span 和 continuity key |
| `SpanId` | 已完成 | 稳定 span identifier，限制为跨平台 ASCII 字符 |
| `TraceFields::from_event` | 已完成 | 从 `eva-core::Event` 提取不含 payload 的链路字段 |
| `AuditAction` | 已完成 V1.16.1 | 定义配置、policy、runtime、event、Lua host log/audit、capability、adapter、provider credential session/supervision、MCP session/stream/proxy、restore apply/rollback、scheduler retry、task lifecycle、hardware driver/hotplug、memory write/read/search/context/maintenance、安全拒绝等稳定动作 |
| `AuditOutcome` | 已完成 | `ok`、`planned`、`blocked`、`failed` |
| `AuditEvent` | 已完成 | 记录动作、结果、trace、消息、扩展字段和时间 |
| `AuditSink` | 已完成 | 抽象审计写入 trait |
| `InMemoryAuditSink` | 已完成 | 测试和 dry-run 可用的内存 sink |
| `MetricName` | 已完成 | 稳定 metric 名称校验 |
| `MetricLabels` | 已完成 V1.16.3 | 使用 `BTreeMap` 保证标签顺序稳定；提供 runtime/provider/task 标签 helper 和稳定顺序的 label cardinality limit |
| `MetricPoint` | 已完成 | 表示一个 counter/gauge/histogram 数据点 |
| `FileObservabilitySink` | 已完成 V1.16.4 | 写入 `audit.jsonl`、`metrics.jsonl` 和 `otel-spans.jsonl`；显式 policy 可按 max size 轮转、扫描损坏 JSONL 行并只删除过期轮转文件 |
| `BestEffortObservabilityPipeline` | 已完成 V1.9.5 | 后端不可用或写入失败时降级到 in-memory audit/metrics，并记录 degraded reason |
| retention policy | 已完成 V1.16.4 | `ObservabilityRetentionPolicy` 支持 `jsonl-file`、`durable-audit` 和 `database` policy kind；当前真实执行覆盖 JSONL 文件轮转/保留和 durable audit 保留/损坏记录处理，database 仍是配置和策略边界 |
| 具体后端 | 部分实现 V1.16.4 | `eva-storage::FileSystemAuditSink` 可写 durable backend `audit/` 并接入 durable-audit retention policy；本 crate 提供 JSONL 后端、OTel-style span export、rotation/retention/corrupt record report；V1.16.1 已把 daemon/provider/task/restore 关键路径接入 best-effort pipeline；V1.16.2 已接 tracing subscriber bridge、JSONL/dev-console sink 和脱敏；V1.16.3 已接 OpenTelemetry SDK OTLP HTTP/protobuf trace/metrics exporter smoke、collector degraded report 和 label 基数限制；真实 database sink 仍是后续范围 |

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
| `MetricLabels::limited` | max label count | `MetricLabels` | 按稳定排序裁剪 metrics labels，限制 exporter label 基数 |
| `MetricPoint::new` | name、kind、value | `MetricPoint` | 构造指标点 |
| `FileObservabilitySink::open` | backend root | `Result<FileObservabilitySink, EvaError>` | 打开 audit/metrics/span JSONL 后端 |
| `FileObservabilitySink::open_with_policy` | backend root、`ObservabilityRetentionPolicy` | `Result<FileObservabilitySink, EvaError>` | 打开带 JSONL retention/rotation policy 的文件后端 |
| `FileObservabilitySink::apply_retention_policy` | `&self` | `ObservabilityRetentionReport` | 扫描 JSONL 后端，报告损坏记录，删除过期轮转文件 |
| `BestEffortObservabilityPipeline::open` | backend root | `BestEffortObservabilityPipeline` | 打开可降级观测 pipeline |
| `BestEffortObservabilityPipeline::open_with_policy` | backend root、`ObservabilityRetentionPolicy` | `BestEffortObservabilityPipeline` | 打开带 retention/rotation policy 的可降级观测 pipeline |
| `BestEffortObservabilityPipeline::export_span` | span 名称、trace、attributes | `Result<(), EvaError>` | 输出 OTel-style span JSONL |
| `ObservabilityRetentionPolicy::jsonl_file` | none | `ObservabilityRetentionPolicy` | 构造 JSONL 文件 sink 保留策略 |
| `ObservabilityRetentionPolicy::durable_audit` | none | `ObservabilityRetentionPolicy` | 构造 durable audit sink 保留策略 |
| `TracingBridgeLayer::new` | sink、continuity key | `TracingBridgeLayer` | V1.16.2 tracing subscriber layer，把 span/event 映射到 TraceFields/AuditEvent |
| `run_tracing_bridge_smoke` | bridge sink、trace | `TracingBridgeReport` | 验证 JSONL/dev-console bridge、span id 去重和敏感字段脱敏 |
| `OpenTelemetryExporterConfig::new` | endpoint | `OpenTelemetryExporterConfig` | 配置 V1.16.3 OTLP HTTP/protobuf exporter endpoint/auth/batch/timeout/drop policy/label limit |
| `run_opentelemetry_exporter_smoke` | exporter config、trace | `OpenTelemetryExporterReport` | 向 fake/real collector 发送 trace/metrics smoke，collector 不可用时返回 degraded report |

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

- 默认不全局安装 tracing subscriber；只有显式使用 `TracingBridgeLayer` 或 `run_tracing_bridge_smoke` 时才接入 V1.16.2 bridge。
- 不在默认路径隐式写文件；只有显式使用 `FileObservabilitySink` 或 `BestEffortObservabilityPipeline` 时写 JSONL。
- 不默认调用 OpenTelemetry SDK；只有显式使用 `run_opentelemetry_exporter_smoke` 或 CLI `--otel-endpoint` 时才执行 V1.16.3 OTLP exporter smoke。
- 不默认执行 retention/rotation；只有显式使用 `open_with_policy` 或 `apply_retention_policy` 时才处理 JSONL/durable audit 保留策略。
- 不提供真实数据库 sink；`database` 目前只是配置和 policy kind 边界。
- 不提供完整生产保留策略调度；常驻后台调度仍由后续 runtime 接入。
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
| `metric_labels_apply_deterministic_cardinality_limit` | metrics label 基数限制保持稳定顺序 |
| `child_span_preserves_trace_continuity` | child span 和 continuity key 语义 |
| `audit_action_spelling_is_stable` | 覆盖 provider、MCP、hardware 和 memory maintenance audit action 稳定拼写 |
| `opentelemetry_exporter_smoke_reaches_fake_collector` | V1.16.3 SDK OTLP trace/metrics exporter fake collector e2e |
| `opentelemetry_exporter_degrades_when_collector_is_unavailable` | collector 不可用时 exporter report degraded 且调用方可继续 |
| `opentelemetry_exporter_applies_batch_and_label_limits` | exporter batch 和 metrics label 基数限制 |
| `file_observability_policy_rotates_and_continues_writing` | V1.16.4 JSONL max size rotation 后继续写入 |
| `jsonl_retention_deletes_only_expired_observability_files_and_reports_corrupt_records` | V1.16.4 retention 只删除过期轮转文件，并报告损坏 JSONL 行 |

### 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V0.2 | 定义 `TraceFields` 和 `SpanId`，支持从核心事件提取链路字段。 | `eva-core` | 字段名稳定且不包含 payload。 |
| 2 | V0.2 | 定义 `AuditAction`、`AuditOutcome`、`AuditEvent` 和 sink trait。 | `eva-core::EvaError` | audit action 字符串稳定。 |
| 3 | V0.2 | 定义 `MetricName`、`MetricLabels`、`MetricPoint`。 | 标准库 `BTreeMap` | 标签顺序稳定。 |
| 4 | V0.3 | 接 CLI 输出 envelope 和诊断 trace 字段。 | `eva-cli` | human/json 输出共享同一 trace 字段。 |
| 5 | V0.4 | 接 runtime、eventbus、scheduler、agent 的 audit/metrics。 | runtime 主链路 | 事件闭环每个阶段有 trace。 |
| 6 | V1.1+ | 接 adapter、MCP、discovery、hardware、memory、backup、lifecycle 审计动作。 | 扩展模块 | 外部能力和高风险操作可审计；V1.8.3 已加入 MCP session/stream/proxy 动作，V1.13.2 已加入 provider credential session 动作，V1.15.6 已加入 memory maintenance 动作，V1.15.8 已加入 memory write/read/search/context 动作，V1.16.1 已加入 runtime control、task lifecycle、scheduler retry、provider supervised 和 restore apply/rollback 动作。 |
| 7 | V1.9.5 | 接 file JSONL backend、OTel-style span export 和 best-effort pipeline。 | 发布阶段选型 | 后端不改变公共字段契约，后端不可用时不阻塞核心任务。 |
| 8 | V1.16.2 | 接 tracing subscriber bridge、JSONL/dev-console sink 和脱敏。 | `tracing-subscriber` | span/event 可映射到现有 TraceFields/AuditEvent；dev console 不泄漏 secret；span id 去重。 |
| 9 | V1.16.3 | 接 OpenTelemetry SDK OTLP HTTP/protobuf exporter 和 metrics exporter smoke。 | `opentelemetry-otlp` | fake collector e2e；collector 不可用时 degraded；metrics label cardinality 有上限。 |
| 10 | V1.16.4 | 接 JSONL/durable audit retention/rotation、max size 和 corrupt/tamper handling policy。 | 常驻 runtime | JSONL rotation 后可继续写；retention 只删除过期观测数据；损坏记录可 skip-and-report 或 fail-fast；database sink 保留为 policy 边界。 |
| 11 | 后续 | 接真实 database sink 和常驻 runtime retention 调度。 | runtime/storage 选型 | 生产 runtime 可按部署策略管理长期保留。 |

### 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 公共导出 | 已完成 | 随新观测动作扩展 re-export。 |
| `src/trace.rs` | trace 字段、span id、event 提取、child span、continuity key | 已完成 V1.9.5 | V0.4 接 runtime/eventbus/agent span。 |
| `src/audit.rs` | audit action/outcome/event/sink | 已完成 V1.16.1 | 已包含 provider credential session/supervision、restore apply/rollback、scheduler retry、task lifecycle、hardware 和 memory write/read/search/context/maintenance 动作；后续继续追加分发动作。 |
| `src/metrics.rs` | metric name、labels、point、runtime/provider/task labels、label cardinality limit | 已完成 V1.16.3 | V0.4 定义 runtime/eventbus 指标命名。 |
| `src/backend.rs` | file JSONL backend、OTel-style span export、best-effort degradation、smoke report、JSONL rotation/retention/corrupt report | 已完成 V1.16.4 | 已由 runtime/provider/task/restore 路径使用；后续接真实 db sink 和常驻调度。 |
| `src/retention.rs` | retention/rotation/corrupt-record policy 和 report 类型 | 已完成 V1.16.4 | 后续真实 database sink 复用同一 policy kind。 |
| `src/opentelemetry_exporter.rs` | OpenTelemetry SDK OTLP HTTP/protobuf trace/metrics exporter smoke、collector degraded report、label limit | 已完成 V1.16.3 | 后续与 retention/db policy 组合成生产后端策略。 |
| concrete backend | tracing/OpenTelemetry SDK/db/rotation | 部分实现 V1.16.4 | JSONL runtime wiring、OTel exporter smoke 和 JSONL/durable-audit retention policy 已完成；真实 database sink 后续独立选型。 |

## English

`eva-observability` defines shared trace, audit, and metrics contracts. V1.9.5 adds a best-effort file JSONL backend, OTel-style span JSONL export, runtime/provider/task label helpers, and trace continuity helpers. V1.15.8 adds stable memory write/read/search/context actions; V1.16.1 adds stable runtime/provider/task/restore actions and JSONL wiring over that backend. V1.16.2 adds a tracing subscriber bridge that maps spans/events into TraceFields, AuditEvent, and existing JSONL/dev-console sinks with redaction. V1.16.3 adds SDK-based OTLP HTTP/protobuf trace/metrics exporter smoke with collector-degraded reporting and metric label limits. V1.16.4 adds explicit JSONL/durable-audit retention, rotation, max-size, and corrupt-record policy plus a database policy kind boundary. It does not route business logic, authorize requests, or provide a real database sink yet.
