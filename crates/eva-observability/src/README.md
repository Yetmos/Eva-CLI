# eva-observability/src / 可观测性源码

![Eva module implementation roadmap](../../assets/eva-module-implementation-roadmap.svg)

本目录承载 trace、audit、metrics 契约、V1.9.5 file JSONL backend 基线、V1.13.2 provider credential session audit action、V1.15.6 memory maintenance audit action、V1.15.8 memory write/read/search/context audit actions、V1.16.1 runtime/provider/task/restore audit sink wiring 动作、V1.16.2 tracing subscriber bridge 和 V1.16.3 OpenTelemetry SDK exporter smoke。后续模块只应追加稳定动作、指标或后端 adapter，不应改变已公开字段含义。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 公共导出 | 已完成 | V0.2 |
| `trace.rs` | `TraceFields`、`SpanId`、event 字段提取、request-level builder、child span、continuity key | 已完成 V1.9.5 | V0.2/V0.4/P5 |
| `audit.rs` | `AuditAction`、`AuditOutcome`、`AuditEvent`、`AuditSink`；provider credential session/supervision、restore apply/rollback、scheduler retry、task lifecycle、hardware driver/hotplug、memory write/read/search/context/maintenance actions | V1.16.1 已更新 | V0.2/V1.x |
| `metrics.rs` | `MetricName`、`MetricLabels`、`MetricPoint`、runtime/provider/task label helpers、label cardinality limit | 已完成 V1.16.3 | V0.2/V0.4/V1.9.5/V1.16.3 |
| `backend.rs` | `FileObservabilitySink`、`BestEffortObservabilityPipeline`、OTel-style span JSONL export、smoke report | 已完成 V1.9.5 | V1.9.5 |
| `tracing_bridge.rs` | `TracingBridgeLayer`、`TracingBridgeSink`、`TracingBridgeReport`、span/event 到 TraceFields/AuditEvent 映射、JSONL/dev-console sink 和脱敏 | 已完成 V1.16.2 | V1.16.2 |
| `opentelemetry_exporter.rs` | `OpenTelemetryExporterConfig`、`OpenTelemetryDropPolicy`、`OpenTelemetryExporterReport`、OTLP HTTP/protobuf trace/metrics exporter smoke 和 degraded report | 已完成 V1.16.3 | V1.16.3 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义 trace 字段并从 `eva-core::Event` 提取。 | 事件链路可追踪。 |
| 2 | 定义 audit action/outcome 和 sink trait。 | 高风险操作可审计；V1.6.4 追加 `runtime.recovered`，V1.8.3 追加 MCP session/stream/proxy actions，V1.10.2 追加 hardware driver/hotplug actions，V1.13.2 追加 provider credential session action，V1.15.6 追加 memory maintenance action，V1.15.8 追加 memory write/read/search/context actions，V1.16.1 追加 runtime control、task lifecycle、scheduler retry、provider supervised 和 restore apply/rollback actions。 |
| 3 | 定义 metric name、labels、point 和运行面标签 helper。 | 指标命名和标签稳定。 |
| 4 | 增加 file JSONL backend、OTel-style span export 和 best-effort 降级。 | 审计、指标、span 可持久化；后端故障不阻塞调用方。 |
| 5 | V1.16.2 接 tracing subscriber bridge，并把 span/event 映射到现有 TraceFields/AuditEvent。 | JSONL/dev-console bridge 可验证，secret 已脱敏。 |
| 6 | V1.16.3 接 OpenTelemetry SDK OTLP HTTP/protobuf exporter smoke 和 metrics label 上限。 | fake collector e2e 可验证；collector 不可用时 degraded。 |
| 7 | 后续按模块追加 action、metric、retention/rotation 和 db sink policy，不更改已有字段。 | 兼容 CLI 和后端。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Trace | 稳定链路字段 | 已完成 | 接 runtime/eventbus span。 |
| Audit | 审计事件和 sink | 已完成 V1.16.1 | 已包含 memory write/read/search/context；增加后续分发动作。 |
| Metrics | 指标点、runtime/provider/task 标签和 label 基数限制 | 已完成 V1.16.3 | 定义 runtime/eventbus 指标命名。 |
| Backend | file JSONL、OTel-style span export、best-effort degradation | 已完成 V1.16.1 | 已接 runtime/provider/task/restore JSONL wiring；后续接 db sink、retention/rotation。 |
| Tracing bridge | tracing subscriber span/event bridge | 已完成 V1.16.2 | 后续 retention/db policy 复用该映射。 |
| OpenTelemetry exporter | SDK OTLP HTTP/protobuf trace/metrics exporter smoke | 已完成 V1.16.3 | 后续接常驻 runtime exporter 配置和 retention/db policy。 |
