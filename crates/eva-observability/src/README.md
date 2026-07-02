# eva-observability/src / 可观测性源码

## 中文

源码按三类观测契约组织：

| 文件 | 职责 |
| --- | --- |
| `trace.rs` | `TraceFields`、`SpanId` 和从 `eva-core::Event` 提取链路字段 |
| `audit.rs` | `AuditAction`、`AuditOutcome`、`AuditEvent`、`AuditSink` |
| `metrics.rs` | `MetricName`、`MetricLabels`、`MetricKind`、`MetricPoint`、`MetricSink` |

实现约束：

- 字段名必须稳定，后续 CLI JSON、audit 和 metrics 后端复用同一套命名。
- sink trait 只定义写入边界，不选择后端。
- `TraceFields::from_event` 不解释 payload。
- `MetricLabels` 使用有序 map，避免测试、日志和快照输出顺序漂移。

## English

Trace, audit, and metrics contracts are intentionally backend-free. Runtime and CLI code can depend on the shared fields without pulling in a logging implementation.
