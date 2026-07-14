//! 中文：可观察性写入端的保留、轮转和损坏记录处理策略。
//! Retention, rotation, and corrupt-record policy for observability sinks.

use eva_core::EvaError;
use std::fmt;

/// 中文：单个 JSONL 文件轮转前允许的默认最大字节数。
const DEFAULT_MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// 中文：默认最多保留的历史轮转文件数量。
const DEFAULT_MAX_ROTATED_FILES: usize = 16;
/// 中文：默认保留七天的毫秒数。
const DEFAULT_RETAIN_FOR_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// 中文：保留策略适用的可观察性写入端类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservabilitySinkPolicyKind {
    /// 中文：按行写入 JSON 对象的普通文件后端。
    JsonlFile,
    /// 中文：持久化后端中的审计记录存储。
    DurableAudit,
    /// 中文：数据库型可观察性后端。
    Database,
}

/// 中文：读取历史数据遇到损坏记录时的处理方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservabilityCorruptRecordPolicy {
    /// 中文：跳过损坏记录并在报告中保留路径和计数。
    SkipAndReport,
    /// 中文：遇到第一条损坏记录就返回错误，避免静默缺失证据。
    FailFast,
}

/// 中文：单个可观察性写入端的容量、数量、时间和损坏处理上界。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservabilityRetentionPolicy {
    /// 中文：本策略控制的写入端类别。
    pub sink_kind: ObservabilitySinkPolicyKind,
    /// 中文：单文件达到此字节数后应执行轮转。
    pub max_file_bytes: u64,
    /// 中文：允许同时保留的轮转文件上限。
    pub max_rotated_files: usize,
    /// 中文：记录文件允许保留的最长时间毫秒数。
    pub retain_for_ms: u64,
    /// 中文：扫描中遇到损坏记录时的处理策略。
    pub corrupt_record_policy: ObservabilityCorruptRecordPolicy,
}

/// 中文：一次保留和轮转操作的计数、损坏文件及警告摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservabilityRetentionReport {
    /// 中文：应用策略的稳定写入端类别名称。
    pub sink_kind: String,
    /// 中文：本次新轮转的文件数。
    pub rotated_files: usize,
    /// 中文：因数量或时间上限删除的文件数。
    pub deleted_files: usize,
    /// 中文：执行完成后仍保留的文件数。
    pub retained_files: usize,
    /// 中文：在宽容模式中跳过的损坏记录总数。
    pub skipped_corrupt_records: usize,
    /// 中文：去重后的损坏文件路径列表。
    pub corrupt_files: Vec<String>,
    /// 中文：去重后的非致命操作警告。
    pub warnings: Vec<String>,
}

impl Default for ObservabilityRetentionPolicy {
    /// 中文：默认使用 JSONL 文件、8 MiB 轮转、16 个历史文件和七天保留期。
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
    /// 中文：创建默认 JSONL 文件保留策略。
    pub fn jsonl_file() -> Self {
        Self::default()
    }

    /// 中文：创建沿用默认容量与时间上限的持久化审计策略。
    pub fn durable_audit() -> Self {
        Self {
            sink_kind: ObservabilitySinkPolicyKind::DurableAudit,
            ..Self::default()
        }
    }

    /// 中文：创建沿用默认容量与时间上限的数据库策略。
    pub fn database() -> Self {
        Self {
            sink_kind: ObservabilitySinkPolicyKind::Database,
            ..Self::default()
        }
    }

    /// 中文：设置单文件轮转字节上限。
    pub fn with_max_file_bytes(mut self, value: u64) -> Self {
        self.max_file_bytes = value;
        self
    }

    /// 中文：设置轮转历史文件数量上限。
    pub fn with_max_rotated_files(mut self, value: usize) -> Self {
        self.max_rotated_files = value;
        self
    }

    /// 中文：设置记录保留时间上限。
    pub fn with_retain_for_ms(mut self, value: u64) -> Self {
        self.retain_for_ms = value;
        self
    }

    /// 中文：设置损坏记录处理策略。
    pub fn with_corrupt_record_policy(mut self, value: ObservabilityCorruptRecordPolicy) -> Self {
        self.corrupt_record_policy = value;
        self
    }

    /// 中文：验证所有容量和时间上限均非零，避免立即轮转或删除全部数据。
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
    /// 中文：为给定写入端类别创建所有计数为零的报告。
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

    /// 中文：记录损坏文件并累计跳过数量；同一路径只在列表中保留一次。
    pub fn record_corrupt_file(&mut self, path: impl Into<String>, skipped_records: usize) {
        let path = path.into();
        if !self.corrupt_files.contains(&path) {
            self.corrupt_files.push(path);
        }
        self.skipped_corrupt_records += skipped_records;
    }

    /// 中文：追加一条非致命警告，同时避免重复文本污染报告。
    pub fn warn(&mut self, warning: impl Into<String>) {
        let warning = warning.into();
        if !self.warnings.contains(&warning) {
            self.warnings.push(warning);
        }
    }
}

impl ObservabilitySinkPolicyKind {
    /// 中文：解析 CLI 兼容拼写为写入端类别，未知值列出受支持集合。
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

    /// 中文：返回配置和报告使用的规范稳定名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::JsonlFile => "jsonl-file",
            Self::DurableAudit => "durable-audit",
            Self::Database => "database",
        }
    }
}

impl fmt::Display for ObservabilitySinkPolicyKind {
    /// 中文：使用规范稳定名称格式化写入端类别。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl ObservabilityCorruptRecordPolicy {
    /// 中文：解析连字符或下划线兼容拼写为损坏记录策略。
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

    /// 中文：返回配置和报告使用的规范稳定名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SkipAndReport => "skip-and-report",
            Self::FailFast => "fail-fast",
        }
    }
}

impl fmt::Display for ObservabilityCorruptRecordPolicy {
    /// 中文：使用规范稳定名称格式化损坏记录策略。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
