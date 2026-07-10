//! Retention, rotation, and corrupt-record policy for observability sinks.

use eva_core::EvaError;
use std::fmt;

const DEFAULT_MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_MAX_ROTATED_FILES: usize = 16;
const DEFAULT_RETAIN_FOR_MS: u64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservabilitySinkPolicyKind {
    JsonlFile,
    DurableAudit,
    Database,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservabilityCorruptRecordPolicy {
    SkipAndReport,
    FailFast,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservabilityRetentionPolicy {
    pub sink_kind: ObservabilitySinkPolicyKind,
    pub max_file_bytes: u64,
    pub max_rotated_files: usize,
    pub retain_for_ms: u64,
    pub corrupt_record_policy: ObservabilityCorruptRecordPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservabilityRetentionReport {
    pub sink_kind: String,
    pub rotated_files: usize,
    pub deleted_files: usize,
    pub retained_files: usize,
    pub skipped_corrupt_records: usize,
    pub corrupt_files: Vec<String>,
    pub warnings: Vec<String>,
}

impl Default for ObservabilityRetentionPolicy {
    fn default() -> Self {
        Self {
            sink_kind: ObservabilitySinkPolicyKind::JsonlFile,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_rotated_files: DEFAULT_MAX_ROTATED_FILES,
            retain_for_ms: DEFAULT_RETAIN_FOR_MS,
            corrupt_record_policy: ObservabilityCorruptRecordPolicy::SkipAndReport,
        }
    }
}

impl ObservabilityRetentionPolicy {
    pub fn jsonl_file() -> Self {
        Self::default()
    }

    pub fn durable_audit() -> Self {
        Self {
            sink_kind: ObservabilitySinkPolicyKind::DurableAudit,
            ..Self::default()
        }
    }

    pub fn database() -> Self {
        Self {
            sink_kind: ObservabilitySinkPolicyKind::Database,
            ..Self::default()
        }
    }

    pub fn with_max_file_bytes(mut self, value: u64) -> Self {
        self.max_file_bytes = value;
        self
    }

    pub fn with_max_rotated_files(mut self, value: usize) -> Self {
        self.max_rotated_files = value;
        self
    }

    pub fn with_retain_for_ms(mut self, value: u64) -> Self {
        self.retain_for_ms = value;
        self
    }

    pub fn with_corrupt_record_policy(mut self, value: ObservabilityCorruptRecordPolicy) -> Self {
        self.corrupt_record_policy = value;
        self
    }

    pub fn validate(&self) -> Result<(), EvaError> {
        if self.max_file_bytes == 0 {
            return Err(EvaError::invalid_argument(
                "observability retention max_file_bytes must be greater than zero",
            ));
        }
        if self.max_rotated_files == 0 {
            return Err(EvaError::invalid_argument(
                "observability retention max_rotated_files must be greater than zero",
            ));
        }
        if self.retain_for_ms == 0 {
            return Err(EvaError::invalid_argument(
                "observability retention retain_for_ms must be greater than zero",
            ));
        }
        Ok(())
    }
}

impl ObservabilityRetentionReport {
    pub fn new(kind: ObservabilitySinkPolicyKind) -> Self {
        Self {
            sink_kind: kind.as_str().to_owned(),
            rotated_files: 0,
            deleted_files: 0,
            retained_files: 0,
            skipped_corrupt_records: 0,
            corrupt_files: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn record_corrupt_file(&mut self, path: impl Into<String>, skipped_records: usize) {
        let path = path.into();
        if !self.corrupt_files.contains(&path) {
            self.corrupt_files.push(path);
        }
        self.skipped_corrupt_records += skipped_records;
    }

    pub fn warn(&mut self, warning: impl Into<String>) {
        let warning = warning.into();
        if !self.warnings.contains(&warning) {
            self.warnings.push(warning);
        }
    }
}

impl ObservabilitySinkPolicyKind {
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "jsonl" | "jsonl-file" | "jsonl_file" => Ok(Self::JsonlFile),
            "durable-audit" | "durable_audit" => Ok(Self::DurableAudit),
            "database" | "db" => Ok(Self::Database),
            _ => Err(
                EvaError::invalid_argument("unknown observability sink policy kind")
                    .with_context("value", value)
                    .with_context("expected", "jsonl-file|durable-audit|database"),
            ),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::JsonlFile => "jsonl-file",
            Self::DurableAudit => "durable-audit",
            Self::Database => "database",
        }
    }
}

impl fmt::Display for ObservabilitySinkPolicyKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl ObservabilityCorruptRecordPolicy {
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "skip-and-report" | "skip_and_report" => Ok(Self::SkipAndReport),
            "fail-fast" | "fail_fast" => Ok(Self::FailFast),
            _ => Err(
                EvaError::invalid_argument("unknown observability corrupt record policy")
                    .with_context("value", value)
                    .with_context("expected", "skip-and-report|fail-fast"),
            ),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SkipAndReport => "skip-and-report",
            Self::FailFast => "fail-fast",
        }
    }
}

impl fmt::Display for ObservabilityCorruptRecordPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
