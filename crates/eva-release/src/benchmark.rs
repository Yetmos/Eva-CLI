//! Production benchmark evidence verification contracts.

use crate::checklist::ReleaseGateStatus;
use crate::performance::{PerformanceBaselineReport, PerformanceBudget};
use eva_core::EvaError;
use std::collections::{BTreeMap, BTreeSet};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "production benchmark evidence contract";

pub const BENCHMARK_EVIDENCE_FORMAT: &str = "eva.release.benchmark_evidence.v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseBenchmarkMeasurement {
    pub component: String,
    pub metric: String,
    pub budget_ms: u64,
    pub observed_ms: u64,
    pub sample_count: u64,
    pub command: String,
    pub environment: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseBenchmarkEvidence {
    pub format: String,
    pub version: String,
    pub source_tag: String,
    pub source_commit: String,
    pub benchmark_status: String,
    pub measurements: Vec<ReleaseBenchmarkMeasurement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseBenchmarkVerificationReport {
    pub status: String,
    pub version: String,
    pub source_tag: String,
    pub source_commit: String,
    pub benchmark_status: String,
    pub measurements: Vec<ReleaseBenchmarkMeasurement>,
    pub regressions: Vec<ReleaseBenchmarkMeasurement>,
    pub risks: Vec<String>,
    pub audit: Vec<String>,
}

impl ReleaseBenchmarkMeasurement {
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

    pub fn status(&self) -> ReleaseGateStatus {
        if self.observed_ms <= self.budget_ms {
            ReleaseGateStatus::Pass
        } else {
            ReleaseGateStatus::Blocked
        }
    }

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

fn required(fields: &BTreeMap<String, String>, key: &str) -> Result<String, EvaError> {
    fields.get(key).cloned().ok_or_else(|| {
        EvaError::invalid_argument("release benchmark evidence is missing required field")
            .with_context("required_field", key)
    })
}

fn required_indexed(
    fields: &BTreeMap<String, String>,
    prefix: &str,
    index: usize,
    field: &str,
) -> Result<String, EvaError> {
    required(fields, &format!("{prefix}.{index}.{field}"))
}

fn indexed_fields(fields: &BTreeMap<String, String>, prefix: &str) -> BTreeSet<usize> {
    fields
        .keys()
        .filter_map(|key| key.strip_prefix(&format!("{prefix}.")))
        .filter_map(|remaining| remaining.split_once('.').map(|(index, _)| index))
        .filter_map(|index| index.parse::<usize>().ok())
        .collect()
}

fn parse_u64(value: String) -> Result<u64, EvaError> {
    value.parse::<u64>().map_err(|error| {
        EvaError::invalid_argument("benchmark integer field must be a u64")
            .with_context("value", value)
            .with_context("parse_error", error.to_string())
    })
}

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

fn validate_non_empty(field: &str, value: String) -> Result<String, EvaError> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(
            EvaError::invalid_argument(format!("{field} must be non-empty and trimmed"))
                .with_context("value", value),
        );
    }
    Ok(value)
}

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
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

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
    fn benchmark_evidence_can_feed_performance_report() {
        let report = evidence("passed", 120).to_performance_report();

        assert_eq!(report.status, "within_budget");
        assert_eq!(report.over_budget_count(), 0);
        assert_eq!(report.budgets[0].component, "release.check");
    }
}
