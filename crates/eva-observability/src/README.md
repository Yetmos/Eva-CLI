# eva-observability/src / 可观测性源码

![Eva module implementation roadmap](../../assets/eva-module-implementation-roadmap.svg)

本目录承载 trace、audit 和 metrics 契约。V0.2 已完成公共字段和内存 sink，后续模块只应追加稳定动作或指标，不应改变已公开字段含义。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 公共导出 | 已完成 | V0.2 |
| `trace.rs` | `TraceFields`、`SpanId`、event 字段提取、request-level builder | 已完成 | V0.2/V0.4/P5 |
| `audit.rs` | `AuditAction`、`AuditOutcome`、`AuditEvent`、`AuditSink` | V1.6.4 已更新 | V0.2/V1.x |
| `metrics.rs` | `MetricName`、`MetricLabels`、`MetricPoint` | 已完成 | V0.2/V0.4 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义 trace 字段并从 `eva-core::Event` 提取。 | 事件链路可追踪。 |
| 2 | 定义 audit action/outcome 和 sink trait。 | 高风险操作可审计；V1.6.4 追加 `runtime.recovered` 用于 durable recovery audit。 |
| 3 | 定义 metric name、labels、point。 | 指标命名稳定。 |
| 4 | 后续按模块追加 action 和 metric，不更改已有字段。 | 兼容 CLI 和后端。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Trace | 稳定链路字段 | 已完成 | 接 runtime/eventbus span。 |
| Audit | 审计事件和 sink | 已完成 | 增加 V1.x 高风险动作。 |
| Metrics | 指标点和标签 | 已完成 | 定义 runtime/eventbus 指标命名。 |
| Backend | tracing/OpenTelemetry/file/db | 未实现 | V1.5 发布前选型。 |
