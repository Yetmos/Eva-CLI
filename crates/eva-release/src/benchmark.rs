//! 生产基准测试证据的解析与验证契约。
//! Production benchmark evidence verification contracts.

use crate::checklist::ReleaseGateStatus;
use crate::evidence::{EvidenceEnvelope, EvidenceKind};
use crate::performance::{PerformanceBaselineReport, PerformanceBudget};
use eva_core::EvaError;
use std::collections::{BTreeMap, BTreeSet};

/// 本模块的架构职责：将可复现的性能测量绑定到发布来源并生成预算门禁。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "production benchmark evidence contract";

/// 当前支持的基准测试证据清单格式。
pub const BENCHMARK_EVIDENCE_FORMAT: &str = "eva.release.benchmark_evidence.v1";

/// 一个组件指标的生产环境基准测量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseBenchmarkMeasurement {
    /// 被测组件或操作的稳定键。
    pub component: String,
    /// 被测指标的人类可读名称。
    pub metric: String,
    /// 发布允许的最大毫秒数。
    pub budget_ms: u64,
    /// 本次证据观测到的毫秒数。
    pub observed_ms: u64,
    /// 聚合观测使用的非零样本数。
    pub sample_count: u64,
    /// 生成测量的具体命令。
    pub command: String,
    /// 运行命令的机器或 CI 环境描述。
    pub environment: String,
}

/// 与版本、标签和完整提交绑定的基准测试证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseBenchmarkEvidence {
    /// 证据清单格式版本。
    pub format: String,
    /// 被测发布版本。
    pub version: String,
    /// 被测来源标签。
    pub source_tag: String,
    /// 被测来源完整提交哈希。
    pub source_commit: String,
    /// 基准测试任务的总体状态。
    pub benchmark_status: String,
    /// 一项或多项生产测量。
    pub measurements: Vec<ReleaseBenchmarkMeasurement>,
}

/// 基准证据的预算回归验证结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseBenchmarkVerificationReport {
    /// `verified` 或 `blocked` 状态。
    pub status: String,
    /// 证据对应发布版本。
    pub version: String,
    /// 证据对应来源标签。
    pub source_tag: String,
    /// 证据对应来源提交。
    pub source_commit: String,
    /// 原始基准任务状态。
    pub benchmark_status: String,
    /// 全部测量。
    pub measurements: Vec<ReleaseBenchmarkMeasurement>,
    /// 观测值超过预算的测量。
    pub regressions: Vec<ReleaseBenchmarkMeasurement>,
    /// 缺失、失败或回归带来的具体风险。
    pub risks: Vec<String>,
    /// 来源、总体状态和逐测量审计记录。
    pub audit: Vec<String>,
}

impl ReleaseBenchmarkMeasurement {
    /// 校验组件、非零预算、样本数和环境后创建测量。
    pub fn new(
        component: impl Into<String>,
        metric: impl Into<String>,
        budget_ms: u64,
        observed_ms: u64,
        sample_count: u64,
        command: impl Into<String>,
        environment: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let component = validate_metric_key("benchmark component", component.into())?;
        let metric = validate_non_empty("benchmark metric", metric.into())?;
        if budget_ms == 0 {
            return Err(EvaError::invalid_argument(
                "benchmark budget must be greater than zero",
            ));
        }
        if sample_count == 0 {
            return Err(EvaError::invalid_argument(
                "benchmark sample count must be greater than zero",
            ));
        }
        let command = validate_non_empty("benchmark command", command.into())?;
        let environment = validate_non_empty("benchmark environment", environment.into())?;
        Ok(Self {
            component,
            metric,
            budget_ms,
            observed_ms,
            sample_count,
            command,
            environment,
        })
    }

    /// 根据观测值是否超过预算计算发布门禁状态。
    pub fn status(&self) -> ReleaseGateStatus {
        if self.observed_ms <= self.budget_ms {
            ReleaseGateStatus::Pass
        } else {
            ReleaseGateStatus::Blocked
        }
    }

    /// 转换为通用性能预算，同时保留命令、样本数和环境证据。
    pub fn to_budget(&self) -> PerformanceBudget {
        PerformanceBudget::new(
            self.component.clone(),
            self.metric.clone(),
            self.budget_ms,
            self.observed_ms,
            format!(
                "{}; samples={}; environment={}",
                self.command, self.sample_count, self.environment
            ),
        )
    }
}

impl ReleaseBenchmarkEvidence {
    /// 创建与发布来源绑定且状态受约束的基准证据。
    pub fn new(
        version: impl Into<String>,
        source_tag: impl Into<String>,
        source_commit: impl Into<String>,
        benchmark_status: impl Into<String>,
        measurements: Vec<ReleaseBenchmarkMeasurement>,
    ) -> Result<Self, EvaError> {
        let version = validate_version(version.into())?;
        let source_tag = validate_token("release source tag", source_tag.into())?;
        let source_commit = validate_commit(source_commit.into())?;
        let benchmark_status = validate_status(benchmark_status.into())?;
        Ok(Self {
            format: BENCHMARK_EVIDENCE_FORMAT.to_owned(),
            version,
            source_tag,
            source_commit,
            benchmark_status,
            measurements,
        })
    }

    /// 将规范化 benchmark evidence 文档绑定到统一信封。
    ///
    /// 当前 subject 是 `to_manifest()` 的精确 UTF-8 字节；后续真实 stdout/stderr
    /// capture 应直接使用 `EvidenceEnvelope::from_subject_bytes`，不能把 command 字符串
    /// 当作执行输出。
    pub fn to_envelope(
        &self,
        kind: EvidenceKind,
        source: impl Into<String>,
        environment: impl Into<String>,
        executor: impl Into<String>,
        timestamp: u128,
    ) -> Result<EvidenceEnvelope, EvaError> {
        let subject = self.to_manifest();
        let reparsed = Self::parse_manifest(&subject)?;
        if &reparsed != self {
            return Err(EvaError::invalid_argument(
                "release benchmark evidence manifest is not canonical",
            ));
        }
        EvidenceEnvelope::from_subject_bytes(
            kind,
            source,
            self.source_commit.clone(),
            environment,
            executor,
            timestamp,
            subject.as_bytes(),
        )
    }

    /// 从严格键值清单解析测量，并通过构造器重新执行全部约束。
    ///
    /// 重复字段、缺失索引字段、未知格式和非法数值均失败关闭；数字索引排序保证
    /// 测量顺序稳定。
    pub fn parse_manifest(data: &str) -> Result<Self, EvaError> {
        let fields = parse_key_value_manifest(data)?;
        if required(&fields, "format")? != BENCHMARK_EVIDENCE_FORMAT {
            return Err(EvaError::invalid_argument(
                "unsupported release benchmark evidence format",
            )
            .with_context("format", required(&fields, "format")?));
        }

        let measurements = indexed_fields(&fields, "measurement")
            .into_iter()
            .map(|index| {
                ReleaseBenchmarkMeasurement::new(
                    required_indexed(&fields, "measurement", index, "component")?,
                    required_indexed(&fields, "measurement", index, "metric")?,
                    parse_u64(required_indexed(
                        &fields,
                        "measurement",
                        index,
                        "budget_ms",
                    )?)?,
                    parse_u64(required_indexed(
                        &fields,
                        "measurement",
                        index,
                        "observed_ms",
                    )?)?,
                    parse_u64(required_indexed(
                        &fields,
                        "measurement",
                        index,
                        "sample_count",
                    )?)?,
                    required_indexed(&fields, "measurement", index, "command")?,
                    required_indexed(&fields, "measurement", index, "environment")?,
                )
            })
            .collect::<Result<Vec<_>, EvaError>>()?;

        Self::new(
            required(&fields, "version")?,
            required(&fields, "source_tag")?,
            required(&fields, "source_commit")?,
            required(&fields, "benchmark_status")?,
            measurements,
        )
    }

    /// 验证任务状态、非空测量集合以及每项预算。
    ///
    /// 只有任务状态明确为 passed、至少有一个测量且没有超预算项时才通过。空证据
    /// 或 skipped 状态不会因为“没有回归”而被误判为成功。
    pub fn verify(&self) -> ReleaseBenchmarkVerificationReport {
        let mut risks = Vec::new();
        if self.benchmark_status != "passed" {
            risks.push(format!(
                "benchmark evidence status is {}",
                self.benchmark_status
            ));
        }
        if self.measurements.is_empty() {
            risks.push("benchmark evidence has no measurements".to_owned());
        }

        let regressions = self
            .measurements
            .iter()
            .filter(|measurement| measurement.status().is_blocking())
            .cloned()
            .collect::<Vec<_>>();
        for measurement in &regressions {
            risks.push(format!(
                "benchmark {} observed {}ms over {}ms budget",
                measurement.component, measurement.observed_ms, measurement.budget_ms
            ));
        }

        let status = if risks.is_empty() {
            "verified"
        } else {
            "blocked"
        }
        .to_owned();
        let mut audit = vec![
            "release.benchmark:manifest_parsed".to_owned(),
            format!("release.benchmark.source_commit:{}", self.source_commit),
            format!("release.benchmark.status:{}", self.benchmark_status),
        ];
        audit.extend(self.measurements.iter().map(|measurement| {
            format!(
                "release.benchmark.measurement:{}:{}ms/{}ms:{}samples",
                measurement.component,
                measurement.observed_ms,
                measurement.budget_ms,
                measurement.sample_count
            )
        }));

        ReleaseBenchmarkVerificationReport {
            status,
            version: self.version.clone(),
            source_tag: self.source_tag.clone(),
            source_commit: self.source_commit.clone(),
            benchmark_status: self.benchmark_status.clone(),
            measurements: self.measurements.clone(),
            regressions,
            risks,
            audit,
        }
    }

    /// 将基准测量转换为发布性能基线报告。
    ///
    /// 转换沿用相同的 passed、非空和全部预算内条件，保证两个报告表面结论一致。
    pub fn to_performance_report(&self) -> PerformanceBaselineReport {
        let budgets = self
            .measurements
            .iter()
            .map(ReleaseBenchmarkMeasurement::to_budget)
            .collect::<Vec<_>>();
        let status = if self.benchmark_status == "passed"
            && !budgets.is_empty()
            && budgets.iter().all(PerformanceBudget::within_budget)
        {
            "within_budget"
        } else {
            "over_budget"
        }
        .to_owned();
        PerformanceBaselineReport {
            version: self.version.clone(),
            status,
            budgets,
            audit: vec![
                "performance:benchmark_evidence:v1.11.3".to_owned(),
                format!("source_commit:{}", self.source_commit),
                format!("benchmark_status:{}", self.benchmark_status),
            ],
        }
    }

    /// 以稳定字段和测量顺序序列化基准证据清单。
    pub fn to_manifest(&self) -> String {
        let mut lines = vec![
            format!("format={}", self.format),
            format!("version={}", self.version),
            format!("source_tag={}", self.source_tag),
            format!("source_commit={}", self.source_commit),
            format!("benchmark_status={}", self.benchmark_status),
        ];
        for (index, measurement) in self.measurements.iter().enumerate() {
            lines.push(format!(
                "measurement.{index}.component={}",
                measurement.component
            ));
            lines.push(format!("measurement.{index}.metric={}", measurement.metric));
            lines.push(format!(
                "measurement.{index}.budget_ms={}",
                measurement.budget_ms
            ));
            lines.push(format!(
                "measurement.{index}.observed_ms={}",
                measurement.observed_ms
            ));
            lines.push(format!(
                "measurement.{index}.sample_count={}",
                measurement.sample_count
            ));
            lines.push(format!(
                "measurement.{index}.command={}",
                measurement.command
            ));
            lines.push(format!(
                "measurement.{index}.environment={}",
                measurement.environment
            ));
        }
        format!("{}\n", lines.join("\n"))
    }
}

/// 解析允许 BOM、空行和注释的严格键值清单，并拒绝重复键。
fn parse_key_value_manifest(data: &str) -> Result<BTreeMap<String, String>, EvaError> {
    let mut fields = BTreeMap::new();
    for line in data.lines() {
        let line = line.trim_start_matches('\u{feff}');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            return Err(EvaError::invalid_argument(
                "release benchmark evidence line must use key=value format",
            ));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(EvaError::invalid_argument(
                "release benchmark evidence key cannot be empty",
            ));
        }
        if fields
            .insert(key.to_owned(), value.trim().to_owned())
            .is_some()
        {
            return Err(EvaError::invalid_argument(
                "release benchmark evidence field is duplicated",
            )
            .with_context("field", key));
        }
    }
    Ok(fields)
}

/// 读取必填基准证据字段。
fn required(fields: &BTreeMap<String, String>, key: &str) -> Result<String, EvaError> {
    fields.get(key).cloned().ok_or_else(|| {
        EvaError::invalid_argument("release benchmark evidence is missing required field")
            .with_context("required_field", key)
    })
}

/// 读取指定测量索引下的必填复合字段。
fn required_indexed(
    fields: &BTreeMap<String, String>,
    prefix: &str,
    index: usize,
    field: &str,
) -> Result<String, EvaError> {
    required(fields, &format!("{prefix}.{index}.{field}"))
}

/// 从复合键收集指定前缀下的有效数字索引并排序去重。
fn indexed_fields(fields: &BTreeMap<String, String>, prefix: &str) -> BTreeSet<usize> {
    fields
        .keys()
        .filter_map(|key| key.strip_prefix(&format!("{prefix}.")))
        .filter_map(|remaining| remaining.split_once('.').map(|(index, _)| index))
        .filter_map(|index| index.parse::<usize>().ok())
        .collect()
}

/// 解析无符号 64 位基准数值。
fn parse_u64(value: String) -> Result<u64, EvaError> {
    value.parse::<u64>().map_err(|error| {
        EvaError::invalid_argument("benchmark integer field must be a u64")
            .with_context("value", value)
            .with_context("parse_error", error.to_string())
    })
}

/// 校验基准任务状态属于受支持集合。
fn validate_status(value: String) -> Result<String, EvaError> {
    let value = validate_token("benchmark status", value)?;
    if matches!(value.as_str(), "passed" | "failed" | "blocked" | "skipped") {
        Ok(value)
    } else {
        Err(EvaError::invalid_argument(
            "benchmark status must be passed, failed, blocked, or skipped",
        )
        .with_context("status", value))
    }
}

/// 校验发布版本为非空且不含空白的单个标记。
fn validate_version(value: String) -> Result<String, EvaError> {
    let value = validate_non_empty("release version", value)?;
    if value.contains(char::is_whitespace) {
        return Err(
            EvaError::invalid_argument("release version cannot contain whitespace")
                .with_context("version", value),
        );
    }
    Ok(value)
}

/// 校验组件键不会包含路径分隔符或遍历片段。
fn validate_metric_key(field: &str, value: String) -> Result<String, EvaError> {
    let value = validate_token(field, value)?;
    if value.contains('/') || value.contains('\\') || value.contains("..") {
        return Err(
            EvaError::invalid_argument("benchmark component must be a stable metric key")
                .with_context("component", value),
        );
    }
    Ok(value)
}

/// 校验不允许包含空白的稳定证据标记。
fn validate_token(field: &str, value: String) -> Result<String, EvaError> {
    let value = validate_non_empty(field, value)?;
    if value.contains(char::is_whitespace) {
        return Err(
            EvaError::invalid_argument(format!("{field} cannot contain whitespace"))
                .with_context("value", value),
        );
    }
    Ok(value)
}

/// 校验文本非空且首尾无空白。
fn validate_non_empty(field: &str, value: String) -> Result<String, EvaError> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(
            EvaError::invalid_argument(format!("{field} must be non-empty and trimmed"))
                .with_context("value", value),
        );
    }
    if value.chars().any(|ch| matches!(ch, '\r' | '\n' | '\0')) {
        return Err(
            EvaError::invalid_argument(format!("{field} must fit on one manifest line"))
                .with_context("value", value),
        );
    }
    Ok(value)
}

/// 校验来源提交为完整 40 字符十六进制哈希。
fn validate_commit(value: String) -> Result<String, EvaError> {
    let value = validate_token("release source commit", value)?;
    if value.len() != 40 || !value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(EvaError::invalid_argument(
            "release source commit must be a full 40-character hex sha",
        )
        .with_context("source_commit", value));
    }
    Ok(value)
}

#[cfg(test)]
/// 基准证据清单往返、预算回归和报告转换测试。
mod tests {
    use super::*;

    /// 测试证据使用的完整来源提交。
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    /// 构造具有指定观测值的测试测量。
    fn measurement(observed_ms: u64) -> ReleaseBenchmarkMeasurement {
        ReleaseBenchmarkMeasurement::new(
            "release.check",
            "cli release check wall time",
            200,
            observed_ms,
            3,
            "target/release/eva release check --output json",
            "github-actions-ubuntu-latest",
        )
        .unwrap()
    }

    /// 构造指定任务状态和观测值的测试证据。
    fn evidence(status: &str, observed_ms: u64) -> ReleaseBenchmarkEvidence {
        ReleaseBenchmarkEvidence::new(
            "1.11.5-alpha",
            "v1.11.5-alpha",
            COMMIT,
            status,
            vec![measurement(observed_ms)],
        )
        .unwrap()
    }

    #[test]
    /// 验证预算内证据可往返清单并通过门禁。
    fn benchmark_evidence_round_trips_and_verifies() {
        let parsed =
            ReleaseBenchmarkEvidence::parse_manifest(&evidence("passed", 120).to_manifest())
                .unwrap();

        let report = parsed.verify();

        assert_eq!(report.status, "verified");
        assert!(report.regressions.is_empty());
        assert!(report.risks.is_empty());
    }

    #[test]
    /// 验证超预算测量被列为回归并阻塞发布。
    fn benchmark_regression_blocks_verification() {
        let report = evidence("passed", 250).verify();

        assert_eq!(report.status, "blocked");
        assert_eq!(report.regressions.len(), 1);
        assert!(report
            .risks
            .iter()
            .any(|risk| risk == "benchmark release.check observed 250ms over 200ms budget"));
    }

    #[test]
    /// 验证基准证据转换后的性能报告保持相同预算结论。
    fn benchmark_evidence_can_feed_performance_report() {
        let report = evidence("passed", 120).to_performance_report();

        assert_eq!(report.status, "within_budget");
        assert_eq!(report.over_budget_count(), 0);
        assert_eq!(report.budgets[0].component, "release.check");
    }

    #[test]
    /// 验证 benchmark canonical manifest 可由统一信封独立重算摘要。
    fn benchmark_manifest_binds_to_evidence_envelope() {
        let evidence = evidence("passed", 120);
        let manifest = evidence.to_manifest();
        let envelope = evidence
            .to_envelope(
                EvidenceKind::Measurement,
                "benchmark-run",
                "github-actions-ubuntu-latest",
                "github-actions:run-123",
                1_784_073_600_000,
            )
            .unwrap();

        let report = envelope
            .verify_subject(COMMIT, manifest.as_bytes())
            .unwrap();

        assert!(report.is_verified());
    }

    #[test]
    /// 验证 benchmark 文本字段不能注入额外 measurement 行。
    fn benchmark_manifest_rejects_line_injection() {
        let error = ReleaseBenchmarkMeasurement::new(
            "release.check",
            "wall time\nmeasurement.1.component=forged",
            200,
            120,
            3,
            "eva release check",
            "github-actions-ubuntu-latest",
        )
        .unwrap_err();

        assert_eq!(
            error.message(),
            "benchmark metric must fit on one manifest line"
        );

        let mut mutated = evidence("passed", 120);
        mutated.measurements[0].metric = "wall time\nforged=value".to_owned();
        let error = mutated
            .to_envelope(
                EvidenceKind::Measurement,
                "benchmark-run",
                "github-actions-ubuntu-latest",
                "github-actions:run-123",
                1_784_073_600_000,
            )
            .unwrap_err();
        assert_eq!(
            error.message(),
            "release benchmark evidence manifest is not canonical"
        );
    }
}
