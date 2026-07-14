//! 提供 JSONL 文件后端、保留/轮转策略和不会阻断业务流程的尽力可观测性管道。
//!
//! 文件 sink 的直接接口保留 I/O 错误；尽力管道则把主后端失败记录为降级并把审计、指标
//! 转存内存。JSONL 每条记录追加一行，轮转先重命名当前文件再创建新文件；本模块没有跨
//! sink 或跨进程锁，调用方必须串行化共享路径上的写入与轮转。
//! Best-effort file and OpenTelemetry-style observability backend adapters.

use crate::{
    AuditAction, AuditEvent, AuditOutcome, AuditSink, InMemoryAuditSink, InMemoryMetricSink,
    MetricPoint, MetricSink, ObservabilityCorruptRecordPolicy, ObservabilityRetentionPolicy,
    ObservabilityRetentionReport, ObservabilitySinkPolicyKind, TraceFields,
};
use eva_core::EvaError;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "best-effort production observability backend adapters";

/// 直接向固定 JSONL 文件追加记录，并可在追加前执行大小轮转的文件 sink。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileObservabilitySink {
    /// 记录 `root` 字段对应的值。
    root: PathBuf,
    /// 可选策略控制追加时轮转及显式维护时的过期、损坏记录处理。
    retention_policy: Option<ObservabilityRetentionPolicy>,
}

/// 在主文件后端失败时降级到内存证据、对业务调用始终返回成功的管道。
#[derive(Debug, Clone, PartialEq)]
pub struct BestEffortObservabilityPipeline {
    /// 可用的文件后端；初始化失败时为 `None`，后续不会自动重新打开。
    primary: Option<FileObservabilitySink>,
    /// 记录 `fallback_audit` 字段对应的值。
    fallback_audit: InMemoryAuditSink,
    /// 记录 `fallback_metrics` 字段对应的值。
    fallback_metrics: InMemoryMetricSink,
    /// 去重保存后端失败原因，供健康报告显式暴露数据完整性下降。
    degraded_reasons: Vec<String>,
}

/// 定义 `JSONL_FILES` 常量。
const JSONL_FILES: [&str; 3] = ["audit.jsonl", "metrics.jsonl", "otel-spans.jsonl"];

/// 表示 `ObservabilitySmokeReport` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservabilitySmokeReport {
    /// 记录 `backend_root` 字段对应的值。
    pub backend_root: String,
    /// 记录 `degraded` 字段对应的值。
    pub degraded: bool,
    /// 记录 `degraded_reasons` 字段对应的值。
    pub degraded_reasons: Vec<String>,
    /// 记录 `audit_events` 字段对应的值。
    pub audit_events: usize,
    /// 记录 `metric_points` 字段对应的值。
    pub metric_points: usize,
    /// 记录 `otel_spans` 字段对应的值。
    pub otel_spans: usize,
    /// 记录 `continuity_key` 字段对应的值。
    pub continuity_key: Option<String>,
}

/// 为相关类型实现其约定的行为与方法。
impl FileObservabilitySink {
    /// 打开或读取 `open` 所需的后端数据，失败时保留错误上下文。
    pub fn open(root: impl AsRef<Path>) -> Result<Self, EvaError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(|error| {
            EvaError::internal("failed to create observability backend directory")
                .with_context("path", root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        Ok(Self {
            root,
            retention_policy: None,
        })
    }

    /// 打开或读取 `open_with_policy` 所需的后端数据，失败时保留错误上下文。
    pub fn open_with_policy(
        root: impl AsRef<Path>,
        policy: ObservabilityRetentionPolicy,
    ) -> Result<Self, EvaError> {
        policy.validate()?;
        if policy.sink_kind != ObservabilitySinkPolicyKind::JsonlFile {
            return Err(EvaError::invalid_argument(
                "file observability sink requires jsonl-file retention policy",
            )
            .with_context("sink_kind", policy.sink_kind.as_str()));
        }
        let mut sink = Self::open(root)?;
        sink.retention_policy = Some(policy);
        Ok(sink)
    }

    /// 返回 `root` 对应的数据视图。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 写入或导出 `export_span` 对应的可观测性记录。
    pub fn export_span(
        &mut self,
        name: &str,
        trace: &TraceFields,
        attributes: &[(&str, &str)],
    ) -> Result<(), EvaError> {
        self.append_jsonl("otel-spans.jsonl", &otel_span_json(name, trace, attributes))
    }

    /// 执行 `audit_path` 对应的处理逻辑。
    pub fn audit_path(&self) -> PathBuf {
        self.root.join("audit.jsonl")
    }

    /// 执行 `metrics_path` 对应的处理逻辑。
    pub fn metrics_path(&self) -> PathBuf {
        self.root.join("metrics.jsonl")
    }

    /// 执行 `apply_retention_policy` 对应的受控流程。
    pub fn apply_retention_policy(&self) -> Result<ObservabilityRetentionReport, EvaError> {
        self.apply_retention_policy_at(system_time_millis(SystemTime::now()) as u64)
    }

    /// 执行 `apply_retention_policy_at` 对应的受控流程。
    pub fn apply_retention_policy_at(
        &self,
        now_ms: u64,
    ) -> Result<ObservabilityRetentionReport, EvaError> {
        let policy = self.retention_policy.clone().unwrap_or_default();
        apply_jsonl_retention_policy_at(&self.root, &policy, now_ms)
    }

    /// 在需要时先轮转当前文件，再追加一条带换行的完整 JSONL 记录。
    ///
    /// `&mut self` 串行化单个 sink 实例，但克隆实例和其他进程不共享锁；它们不应并发轮转
    /// 同一路径。轮转成功而追加失败时，旧记录仍保留在轮转文件中，新记录则返回错误。
    fn append_jsonl(&mut self, file_name: &str, line: &str) -> Result<(), EvaError> {
        let path = self.root.join(file_name);
        if let Some(policy) = &self.retention_policy {
            rotate_if_needed(&path, policy, line.len().saturating_add(1) as u64)?;
        }
        append_line(&path, line)
    }
}

/// 为相关类型实现其约定的行为与方法。
impl AuditSink for FileObservabilitySink {
    /// 写入或导出 `record` 对应的可观测性记录。
    fn record(&mut self, event: AuditEvent) -> Result<(), EvaError> {
        self.append_jsonl("audit.jsonl", &audit_event_json(&event))
    }
}

/// 为相关类型实现其约定的行为与方法。
impl MetricSink for FileObservabilitySink {
    /// 写入或导出 `record` 对应的可观测性记录。
    fn record(&mut self, point: MetricPoint) -> Result<(), EvaError> {
        self.append_jsonl("metrics.jsonl", &metric_point_json(&point))
    }
}

/// 为相关类型实现其约定的行为与方法。
impl BestEffortObservabilityPipeline {
    /// 打开或读取 `open` 所需的后端数据，失败时保留错误上下文。
    pub fn open(root: impl AsRef<Path>) -> Self {
        let backend_root = root.as_ref().display().to_string();
        match FileObservabilitySink::open(root) {
            Ok(primary) => Self {
                primary: Some(primary),
                fallback_audit: InMemoryAuditSink::default(),
                fallback_metrics: InMemoryMetricSink::default(),
                degraded_reasons: Vec::new(),
            },
            Err(error) => Self {
                primary: None,
                fallback_audit: degraded_sink_event(format!(
                    "observability backend unavailable at {backend_root}: {}",
                    error.message()
                )),
                fallback_metrics: InMemoryMetricSink::default(),
                degraded_reasons: vec![error.message().to_owned()],
            },
        }
    }

    /// 打开或读取 `open_with_policy` 所需的后端数据，失败时保留错误上下文。
    pub fn open_with_policy(root: impl AsRef<Path>, policy: ObservabilityRetentionPolicy) -> Self {
        let backend_root = root.as_ref().display().to_string();
        match FileObservabilitySink::open_with_policy(root, policy) {
            Ok(primary) => Self {
                primary: Some(primary),
                fallback_audit: InMemoryAuditSink::default(),
                fallback_metrics: InMemoryMetricSink::default(),
                degraded_reasons: Vec::new(),
            },
            Err(error) => Self {
                primary: None,
                fallback_audit: degraded_sink_event(format!(
                    "observability backend unavailable at {backend_root}: {}",
                    error.message()
                )),
                fallback_metrics: InMemoryMetricSink::default(),
                degraded_reasons: vec![error.message().to_owned()],
            },
        }
    }

    /// 执行 `degraded` 对应的处理逻辑。
    pub fn degraded(&self) -> bool {
        !self.degraded_reasons.is_empty()
    }

    /// 执行 `degraded_reasons` 对应的处理逻辑。
    pub fn degraded_reasons(&self) -> &[String] {
        &self.degraded_reasons
    }

    /// 执行 `fallback_audit` 对应的处理逻辑。
    pub fn fallback_audit(&self) -> &InMemoryAuditSink {
        &self.fallback_audit
    }

    /// 执行 `fallback_metrics` 对应的处理逻辑。
    pub fn fallback_metrics(&self) -> &InMemoryMetricSink {
        &self.fallback_metrics
    }

    /// 执行 `backend_root` 对应的处理逻辑。
    pub fn backend_root(&self) -> String {
        self.primary
            .as_ref()
            .map(|sink| sink.root().display().to_string())
            .unwrap_or_else(|| "degraded".to_owned())
    }

    /// 尝试导出 span；失败只登记降级原因并返回 `Ok`，不阻断被观测业务。
    pub fn export_span(
        &mut self,
        name: &str,
        trace: &TraceFields,
        attributes: &[(&str, &str)],
    ) -> Result<(), EvaError> {
        if let Some(primary) = &mut self.primary {
            if let Err(error) = primary.export_span(name, trace, attributes) {
                self.record_degradation(error.message().to_owned());
            }
        } else {
            self.record_degradation("observability backend is unavailable".to_owned());
        }
        Ok(())
    }

    /// 执行 `smoke_report` 对应的处理逻辑。
    pub fn smoke_report(
        &self,
        backend_root: impl Into<String>,
        continuity_key: Option<String>,
    ) -> ObservabilitySmokeReport {
        ObservabilitySmokeReport {
            backend_root: backend_root.into(),
            degraded: self.degraded(),
            degraded_reasons: self.degraded_reasons.clone(),
            audit_events: count_lines(self.primary.as_ref().map(|sink| sink.audit_path()))
                .unwrap_or(self.fallback_audit.events.len()),
            metric_points: count_lines(self.primary.as_ref().map(|sink| sink.metrics_path()))
                .unwrap_or(self.fallback_metrics.points.len()),
            otel_spans: count_lines(
                self.primary
                    .as_ref()
                    .map(|sink| sink.root().join("otel-spans.jsonl")),
            )
            .unwrap_or(0),
            continuity_key,
        }
    }

    /// 去重记录降级原因，并尽力在内存审计中留下后端退化事件。
    fn record_degradation(&mut self, reason: String) {
        if !self.degraded_reasons.contains(&reason) {
            self.degraded_reasons.push(reason.clone());
        }
        let _ = self.fallback_audit.record(
            AuditEvent::new(
                AuditAction::RuntimeRecovered,
                AuditOutcome::Planned,
                TraceFields::default(),
            )
            .with_message("observability backend degraded")
            .with_field("reason", reason),
        );
    }
}

/// 检查活动与轮转文件、删除已过期轮转文件并报告损坏记录。
///
/// `max_rotated_files` 只产生告警，不按数量强制删除；实际删除仅依据 `retain_for_ms`，避免
/// 为满足数量上限而意外丢弃仍在保留期内的证据。无关文件不会被检查或删除。
fn apply_jsonl_retention_policy_at(
    root: &Path,
    policy: &ObservabilityRetentionPolicy,
    now_ms: u64,
) -> Result<ObservabilityRetentionReport, EvaError> {
    policy.validate()?;
    if policy.sink_kind != ObservabilitySinkPolicyKind::JsonlFile {
        return Err(EvaError::invalid_argument(
            "JSONL retention policy requires jsonl-file sink kind",
        )
        .with_context("sink_kind", policy.sink_kind.as_str()));
    }
    let mut report = ObservabilityRetentionReport::new(policy.sink_kind);
    if !root.exists() {
        return Ok(report);
    }

    let mut rotated_by_signal: Vec<(String, u64, PathBuf)> = Vec::new();
    for entry in fs::read_dir(root).map_err(|error| {
        EvaError::internal("failed to read observability backend directory")
            .with_context("path", root.display().to_string())
            .with_context("io_error", error.to_string())
    })? {
        let entry = entry.map_err(|error| {
            EvaError::internal("failed to read observability backend entry")
                .with_context("path", root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };

        if JSONL_FILES.contains(&file_name) {
            inspect_jsonl_file(&path, policy, &mut report)?;
            report.retained_files += 1;
            continue;
        }

        if let Some((signal, rotated_at_ms)) = parse_rotated_jsonl_name(file_name) {
            report.rotated_files += 1;
            inspect_jsonl_file(&path, policy, &mut report)?;
            if rotated_at_ms.saturating_add(policy.retain_for_ms) < now_ms {
                fs::remove_file(&path).map_err(|error| {
                    EvaError::internal("failed to delete expired observability file")
                        .with_context("path", path.display().to_string())
                        .with_context("io_error", error.to_string())
                })?;
                report.deleted_files += 1;
            } else {
                report.retained_files += 1;
                rotated_by_signal.push((signal, rotated_at_ms, path));
            }
        }
    }

    for signal in ["audit", "metrics", "otel-spans"] {
        let count = rotated_by_signal
            .iter()
            .filter(|(candidate, _, _)| candidate == signal)
            .count();
        if count > policy.max_rotated_files {
            report.warn(format!(
                "{signal} rotated file count {count} exceeds max_rotated_files {}",
                policy.max_rotated_files
            ));
        }
    }

    Ok(report)
}

/// 为相关类型实现其约定的行为与方法。
impl AuditSink for BestEffortObservabilityPipeline {
    /// 写入或导出 `record` 对应的可观测性记录。
    fn record(&mut self, event: AuditEvent) -> Result<(), EvaError> {
        if let Some(primary) = &mut self.primary {
            if let Err(error) = AuditSink::record(primary, event.clone()) {
                self.record_degradation(error.message().to_owned());
                self.fallback_audit.record(event)?;
            }
        } else {
            self.fallback_audit.record(event)?;
        }
        Ok(())
    }
}

/// 为相关类型实现其约定的行为与方法。
impl MetricSink for BestEffortObservabilityPipeline {
    /// 写入或导出 `record` 对应的可观测性记录。
    fn record(&mut self, point: MetricPoint) -> Result<(), EvaError> {
        if let Some(primary) = &mut self.primary {
            if let Err(error) = MetricSink::record(primary, point.clone()) {
                self.record_degradation(error.message().to_owned());
                self.fallback_metrics.record(point)?;
            }
        } else {
            self.fallback_metrics.record(point)?;
        }
        Ok(())
    }
}

/// 执行 `degraded_sink_event` 对应的处理逻辑。
fn degraded_sink_event(reason: String) -> InMemoryAuditSink {
    let mut sink = InMemoryAuditSink::default();
    let _ = sink.record(
        AuditEvent::new(
            AuditAction::RuntimeRecovered,
            AuditOutcome::Planned,
            TraceFields::default(),
        )
        .with_message("observability backend degraded")
        .with_field("reason", reason),
    );
    sink
}

/// 以追加模式写入单行并补换行；不执行 fsync，也不提供跨实例锁。
///
/// 进程在写入中崩溃可能留下半行，保留策略会按损坏记录策略报告或失败，而不会自动修复。
fn append_line(path: &Path, line: &str) -> Result<(), EvaError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            EvaError::internal("failed to create observability output directory")
                .with_context("path", parent.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| {
            EvaError::internal("failed to open observability output")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
    writeln!(file, "{line}").map_err(|error| {
        EvaError::internal("failed to write observability output")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })
}

/// 当现有非空文件加上本条记录将超过上限时，先把整个活动文件重命名为唯一轮转名。
fn rotate_if_needed(
    path: &Path,
    policy: &ObservabilityRetentionPolicy,
    incoming_bytes: u64,
) -> Result<(), EvaError> {
    policy.validate()?;
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(());
    };
    if metadata.len() == 0 || metadata.len().saturating_add(incoming_bytes) <= policy.max_file_bytes
    {
        return Ok(());
    }

    let rotated = next_rotated_path(path, system_time_millis(SystemTime::now()) as u64)?;
    fs::rename(path, &rotated).map_err(|error| {
        EvaError::internal("failed to rotate observability output")
            .with_context("from", path.display().to_string())
            .with_context("to", rotated.display().to_string())
            .with_context("io_error", error.to_string())
    })
}

/// 以毫秒时间和最多 999 个碰撞后缀寻找未占用轮转名；耗尽时返回冲突且不改原文件。
fn next_rotated_path(path: &Path, rotated_at_ms: u64) -> Result<PathBuf, EvaError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| EvaError::invalid_argument("observability output path has no file name"))?;
    let signal = file_name
        .strip_suffix(".jsonl")
        .ok_or_else(|| EvaError::invalid_argument("observability output path is not JSONL"))?;

    for suffix in 0..1000 {
        let candidate = if suffix == 0 {
            parent.join(format!("{signal}.{rotated_at_ms}.jsonl"))
        } else {
            parent.join(format!("{signal}.{rotated_at_ms}.{suffix}.jsonl"))
        };
        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(EvaError::conflict(
        "failed to allocate unique observability rotation path",
    ))
}

/// 解析或检查 `parse_rotated_jsonl_name` 对应的数据，并报告无效格式。
fn parse_rotated_jsonl_name(file_name: &str) -> Option<(String, u64)> {
    let base = file_name.strip_suffix(".jsonl")?;
    let parts = base.split('.').collect::<Vec<_>>();
    if parts.len() < 2 {
        return None;
    }
    let signal = parts[0].to_owned();
    if !matches!(signal.as_str(), "audit" | "metrics" | "otel-spans") {
        return None;
    }
    let rotated_at_ms = parts.get(1)?.parse::<u64>().ok()?;
    Some((signal, rotated_at_ms))
}

/// 按行检查 JSON 对象外形，并依策略累计报告或在首个文件检查时失败。
///
/// 这是用于发现空截断、半写等常见损坏的轻量检查，仅验证首尾花括号，不等价于完整 JSON
/// 语法校验；`SkipAndReport` 保留原文件，`FailFast` 也不会删除或改写证据。
fn inspect_jsonl_file(
    path: &Path,
    policy: &ObservabilityRetentionPolicy,
    report: &mut ObservabilityRetentionReport,
) -> Result<(), EvaError> {
    let data = fs::read_to_string(path).map_err(|error| {
        EvaError::internal("failed to read observability JSONL file")
            .with_context("path", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    let corrupt = data
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| !looks_like_json_object(line))
        .count();
    if corrupt == 0 {
        return Ok(());
    }

    match policy.corrupt_record_policy {
        ObservabilityCorruptRecordPolicy::SkipAndReport => {
            report.record_corrupt_file(path.display().to_string(), corrupt);
            Ok(())
        }
        ObservabilityCorruptRecordPolicy::FailFast => Err(EvaError::conflict(
            "observability JSONL file contains corrupt records",
        )
        .with_context("path", path.display().to_string())
        .with_context("corrupt_records", corrupt.to_string())),
    }
}

/// 判断 `looks_like_json_object` 对应的条件是否成立。
fn looks_like_json_object(line: &str) -> bool {
    let value = line.trim();
    value.starts_with('{') && value.ends_with('}')
}

/// 执行 `audit_event_json` 对应的处理逻辑。
fn audit_event_json(event: &AuditEvent) -> String {
    format!(
        "{{\"recorded_at_ms\":{},\"action\":{},\"outcome\":{},\"trace\":{},\"message\":{},\"fields\":{}}}",
        system_time_millis(event.recorded_at),
        json_string(event.action.as_str()),
        json_string(event.outcome.as_str()),
        trace_json(&event.trace),
        option_json(event.message.as_deref()),
        pairs_json(event.fields.iter().map(|(key, value)| (key.as_str(), value.as_str())))
    )
}

/// 执行 `metric_point_json` 对应的处理逻辑。
fn metric_point_json(point: &MetricPoint) -> String {
    format!(
        "{{\"name\":{},\"kind\":{},\"value\":{},\"labels\":{}}}",
        json_string(point.name.as_str()),
        json_string(point.kind.as_str()),
        point.value,
        pairs_json(point.labels.entries())
    )
}

/// 执行 `otel_span_json` 对应的处理逻辑。
fn otel_span_json(name: &str, trace: &TraceFields, attributes: &[(&str, &str)]) -> String {
    format!(
        "{{\"exporter\":\"opentelemetry-jsonl\",\"name\":{},\"trace\":{},\"attributes\":{}}}",
        json_string(name),
        trace_json(trace),
        pairs_json(attributes.iter().copied())
    )
}

/// 执行 `trace_json` 对应的处理逻辑。
fn trace_json(trace: &TraceFields) -> String {
    pairs_json(
        trace
            .entries()
            .iter()
            .map(|(key, value)| (*key, value.as_str())),
    )
}

/// 按稳定格式生成 `pairs_json` 对应的输出。
fn pairs_json<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let entries = pairs
        .into_iter()
        .map(|(key, value)| format!("{}:{}", json_string(key), json_string(value)))
        .collect::<Vec<_>>();
    format!("{{{}}}", entries.join(","))
}

/// 执行 `option_json` 对应的处理逻辑。
fn option_json(value: Option<&str>) -> String {
    value.map(json_string).unwrap_or_else(|| "null".to_owned())
}

/// 执行 `json_string` 对应的处理逻辑。
fn json_string(value: &str) -> String {
    let mut escaped = String::new();
    escaped.push('"');
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

/// 执行 `count_lines` 对应的处理逻辑。
fn count_lines(path: Option<PathBuf>) -> Option<usize> {
    let path = path?;
    let data = fs::read_to_string(path).ok()?;
    Some(data.lines().filter(|line| !line.trim().is_empty()).count())
}

/// 执行 `system_time_millis` 对应的处理逻辑。
fn system_time_millis(value: SystemTime) -> u128 {
    value
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MetricKind, MetricLabels, MetricName, SpanId};

    /// 执行 `temp_root` 对应的处理逻辑。
    fn temp_root(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "eva-observability-{name}-{}-{unique}",
            std::process::id()
        ))
    }

    /// 验证 `file_observability_sink_writes_audit_metrics_and_otel_span` 场景下的预期行为。
    #[test]
    fn file_observability_sink_writes_audit_metrics_and_otel_span() {
        let root = temp_root("file");
        let mut sink = FileObservabilitySink::open(&root).unwrap();
        let trace = TraceFields::default().with_span_id(SpanId::parse("span-obs").unwrap());

        AuditSink::record(
            &mut sink,
            AuditEvent::new(AuditAction::RuntimeStarted, AuditOutcome::Ok, trace.clone())
                .with_message("runtime observed"),
        )
        .unwrap();
        MetricSink::record(
            &mut sink,
            MetricPoint::new(
                MetricName::parse("runtime.events.accepted").unwrap(),
                MetricKind::Counter,
                1.0,
            )
            .with_labels(MetricLabels::runtime("basic", "active")),
        )
        .unwrap();
        sink.export_span("runtime.start", &trace, &[("component", "runtime")])
            .unwrap();

        assert!(root.join("audit.jsonl").is_file());
        assert!(root.join("metrics.jsonl").is_file());
        assert!(root.join("otel-spans.jsonl").is_file());
        fs::remove_dir_all(root).ok();
    }

    /// 验证 `best_effort_pipeline_degrades_without_failing` 场景下的预期行为。
    #[test]
    fn best_effort_pipeline_degrades_without_failing() {
        let root = temp_root("degraded");
        fs::write(&root, b"not a directory").unwrap();
        let mut pipeline = BestEffortObservabilityPipeline::open(&root);

        AuditSink::record(
            &mut pipeline,
            AuditEvent::new(
                AuditAction::RuntimeStarted,
                AuditOutcome::Ok,
                TraceFields::default(),
            ),
        )
        .unwrap();

        assert!(pipeline.degraded());
        assert!(!pipeline.fallback_audit().events.is_empty());
        fs::remove_file(root).ok();
    }

    /// 验证 `file_observability_policy_rotates_and_continues_writing` 场景下的预期行为。
    #[test]
    fn file_observability_policy_rotates_and_continues_writing() {
        let root = temp_root("rotation");
        let policy = ObservabilityRetentionPolicy::jsonl_file()
            .with_max_file_bytes(160)
            .with_max_rotated_files(4);
        let mut sink = FileObservabilitySink::open_with_policy(&root, policy).unwrap();
        let trace = TraceFields::default().with_span_id(SpanId::parse("span-rotate").unwrap());

        for index in 0..4 {
            AuditSink::record(
                &mut sink,
                AuditEvent::new(AuditAction::RuntimeStarted, AuditOutcome::Ok, trace.clone())
                    .with_message(format!("runtime observed {index}")),
            )
            .unwrap();
        }

        assert!(root.join("audit.jsonl").is_file());
        let rotated = fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("audit."))
            .count();
        assert!(rotated > 0, "expected rotated audit JSONL file");
        let report = sink.apply_retention_policy().unwrap();
        assert_eq!(report.deleted_files, 0);
        fs::remove_dir_all(root).ok();
    }

    /// 验证 `jsonl_retention_deletes_only_expired_observability_files_and_reports_corrupt_records` 场景下的预期行为。
    #[test]
    fn jsonl_retention_deletes_only_expired_observability_files_and_reports_corrupt_records() {
        let root = temp_root("retention");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("audit.jsonl"), "{\"active\":true}\n").unwrap();
        fs::write(root.join("audit.1.jsonl"), "{\"expired\":true}\n").unwrap();
        fs::write(root.join("metrics.9000.jsonl"), "not-json\n{\"ok\":true}\n").unwrap();
        fs::write(root.join("unrelated.1.jsonl"), "{\"keep\":true}\n").unwrap();
        let sink = FileObservabilitySink::open_with_policy(
            &root,
            ObservabilityRetentionPolicy::jsonl_file()
                .with_retain_for_ms(5_000)
                .with_corrupt_record_policy(ObservabilityCorruptRecordPolicy::SkipAndReport),
        )
        .unwrap();

        let report = sink.apply_retention_policy_at(10_000).unwrap();

        assert_eq!(report.deleted_files, 1);
        assert_eq!(report.rotated_files, 2);
        assert_eq!(report.skipped_corrupt_records, 1);
        assert!(report
            .corrupt_files
            .iter()
            .any(|path| path.ends_with("metrics.9000.jsonl")));
        assert!(!root.join("audit.1.jsonl").exists());
        assert!(root.join("audit.jsonl").exists());
        assert!(root.join("metrics.9000.jsonl").exists());
        assert!(root.join("unrelated.1.jsonl").exists());
        fs::remove_dir_all(root).ok();
    }
}
