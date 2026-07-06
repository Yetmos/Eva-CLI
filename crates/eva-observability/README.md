# eva-observability / 可观测性

更新时间：2026-07-02

![Eva module implementation roadmap](../assets/eva-module-implementation-roadmap.svg)

`eva-observability` 定义 Eva-CLI 运行时、CLI、Adapter、Agent 和服务模块共享的 trace、audit 与 metrics 契约。它只保存字段、枚举和 sink trait，不接具体日志后端，不做业务路由，也不做权限判断。

## 中文

### 当前实现状态

| 范围 | 状态 | 说明 |
| --- | --- | --- |
| `TraceFields` | 已完成 | 聚合 event、request、topic、agent、adapter、capability、provider、correlation、causation、generation、span；支持 provider invocation trace builder 链式补充 request id |
| `SpanId` | 已完成 | 稳定 span identifier，限制为跨平台 ASCII 字符 |
| `TraceFields::from_event` | 已完成 | 从 `eva-core::Event` 提取不含 payload 的链路字段 |
| `AuditAction` | 已完成 | 定义配置、policy、runtime、event、capability、adapter、安全拒绝等稳定动作 |
| `AuditOutcome` | 已完成 | `ok`、`planned`、`blocked`、`failed` |
| `AuditEvent` | 已完成 | 记录动作、结果、trace、消息、扩展字段和时间 |
| `AuditSink` | 已完成 | 抽象审计写入 trait |
| `InMemoryAuditSink` | 已完成 | 测试和 dry-run 可用的内存 sink |
| `MetricName` | 已完成 | 稳定 metric 名称校验 |
| `MetricLabels` | 已完成 | 使用 `BTreeMap` 保证标签顺序稳定 |
| `MetricPoint` | 已完成 | 表示一个 counter/gauge/histogram 数据点 |
| 具体后端 | 未实现 | 后续版本可接 tracing、OpenTelemetry、文件或数据库 |

### 公开 API

| API | 输入 | 输出 | 用途 |
| --- | --- | --- | --- |
| `TraceFields::from_event` | `&eva_core::Event` | `TraceFields` | 从核心事件构造观测字段 |
| `TraceFields::with_request_id` | `RequestId` | `TraceFields` | 为 CLI/Adapter 调用补充 request-level trace |
| `TraceFields::entries` | `&self` | `Vec<(&'static str, String)>` | 输出当前存在的字段 |
| `SpanId::parse` | `&str` | `Result<SpanId, EvaError>` | 校验 span id |
| `AuditEvent::new` | action、outcome、trace | `AuditEvent` | 构造审计记录 |
| `AuditEvent::with_message` | `impl Into<String>` | `AuditEvent` | 添加人类可读摘要 |
| `AuditEvent::with_field` | key/value | `AuditEvent` | 添加结构化扩展字段 |
| `AuditSink::record` | `AuditEvent` | `Result<(), EvaError>` | 审计写入抽象 |
| `MetricName::parse` | `&str` | `Result<MetricName, EvaError>` | 校验 metric 名称 |
| `MetricLabels::with` | key/value | `MetricLabels` | 添加稳定标签 |
| `MetricPoint::new` | name、kind、value | `MetricPoint` | 构造指标点 |

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

`eva-observability` 不做：

- 不启动 tracing subscriber。
- 不写文件或数据库。
- 不调用 OpenTelemetry。
- 不根据观测结果做 policy 或 routing 决策。
- 不解释 event payload。

### 测试与验证

| 命令 | 当前结果 |
| --- | --- |
| `cargo test -p eva-observability` | 通过，7 个测试 |
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

### 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V0.2 | 定义 `TraceFields` 和 `SpanId`，支持从核心事件提取链路字段。 | `eva-core` | 字段名稳定且不包含 payload。 |
| 2 | V0.2 | 定义 `AuditAction`、`AuditOutcome`、`AuditEvent` 和 sink trait。 | `eva-core::EvaError` | audit action 字符串稳定。 |
| 3 | V0.2 | 定义 `MetricName`、`MetricLabels`、`MetricPoint`。 | 标准库 `BTreeMap` | 标签顺序稳定。 |
| 4 | V0.3 | 接 CLI 输出 envelope 和诊断 trace 字段。 | `eva-cli` | human/json 输出共享同一 trace 字段。 |
| 5 | V0.4 | 接 runtime、eventbus、scheduler、agent 的 audit/metrics。 | runtime 主链路 | 事件闭环每个阶段有 trace。 |
| 6 | V1.1+ | 接 adapter、MCP、discovery、hardware、backup、lifecycle 审计动作。 | 扩展模块 | 外部能力和高风险操作可审计。 |
| 7 | V1.5 | 接具体后端：tracing、OpenTelemetry、文件或数据库。 | 发布阶段选型 | 后端不改变公共字段契约。 |

### 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 公共导出 | 已完成 | 随新观测动作扩展 re-export。 |
| `src/trace.rs` | trace 字段、span id、event 提取 | 已完成 | V0.4 接 runtime/eventbus/agent span。 |
| `src/audit.rs` | audit action/outcome/event/sink | 已完成 | V1.x 增加 Adapter、MCP、backup 等动作。 |
| `src/metrics.rs` | metric name、labels、point | 已完成 | V0.4 定义 runtime/eventbus 指标命名。 |
| `src/README.md` | 源码目录说明 | 简略 | 同步文件职责和后续阶段。 |
| concrete backend | tracing/OpenTelemetry/file/db | 未实现 | V1.5 发布前独立选型。 |

## English

`eva-observability` defines shared trace, audit, and metrics contracts. V0.2 implements `TraceFields`, `SpanId`, `AuditEvent`, `AuditSink`, `MetricName`, `MetricLabels`, and `MetricPoint`. It does not install a logging backend, write files, emit OpenTelemetry data, route business logic, or authorize requests.
