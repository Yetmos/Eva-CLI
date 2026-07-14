//! 基于 durable backend 目录布局的审计事件持久化、保留和 trace 查询。
//! Durable audit sink backed by the filesystem durable backend layout.

use crate::DurableBackendLayout;
use eva_core::EvaError;
use eva_observability::{
    AuditEvent, AuditSink, ObservabilityCorruptRecordPolicy, ObservabilityRetentionPolicy,
    ObservabilityRetentionReport, ObservabilitySinkPolicyKind,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 本模块的架构职责：以单记录文件保存审计事件，并提供稳定序号、保留策略和 trace 查找。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "durable audit event storage and trace lookup";

#[derive(Debug, Clone, PartialEq, Eq)]
/// 审计事件的进程边界快照；trace 与业务 fields 保留原始顺序。
pub struct AuditRecord {
    /// 单调递增的存储序号，同时用于零填充文件名。
    pub sequence: u64,
    /// 事件记录时间的 Unix epoch 毫秒。
    pub recorded_at_ms: u128,
    /// 稳定审计动作码。
    pub action: String,
    /// 稳定审计结果码。
    pub outcome: String,
    /// 可选人类可读说明。
    pub message: Option<String>,
    /// 可查询的 trace 键值，包括 request/span/event/correlation 等标识。
    pub trace: Vec<(String, String)>,
    /// 其余非敏感审计字段。
    pub fields: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 文件系统审计 sink 的内存索引与下一序号状态。
pub struct FileSystemAuditSink {
    /// `.audit` 单记录文件所在目录。
    audit_dir: PathBuf,
    /// 打开时按 sequence 排序加载的有效记录。
    records: Vec<AuditRecord>,
    /// 下一次写入使用的序号；同时考虑有效记录和磁盘上损坏/跳过文件名。
    next_sequence: u64,
}

impl FileSystemAuditSink {
    /// 从 durable layout 的 audit 目录打开严格模式 sink；任何损坏记录都会失败。
    pub fn open(layout: &DurableBackendLayout) -> Result<Self, EvaError> {
        Self::from_audit_dir(&layout.audit_dir)
    }

    /// 从 durable layout 打开并应用保留/损坏记录策略，返回 sink 与清理报告。
    pub fn open_with_policy(
        layout: &DurableBackendLayout,
        policy: &ObservabilityRetentionPolicy,
        now_ms: u128,
    ) -> Result<(Self, ObservabilityRetentionReport), EvaError> {
        Self::from_audit_dir_with_policy(&layout.audit_dir, policy, now_ms)
    }

    /// 从任意 audit 目录打开严格模式 sink，按需创建目录并加载全部记录。
    pub fn from_audit_dir(audit_dir: impl AsRef<Path>) -> Result<Self, EvaError> {
        let audit_dir = audit_dir.as_ref().to_path_buf();
        fs::create_dir_all(&audit_dir).map_err(|error| {
            EvaError::internal("failed to create audit directory")
                .with_context("path", audit_dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let records = load_records(&audit_dir)?;
        let next_sequence = next_sequence_after(&audit_dir, &records)?;
        Ok(Self {
            audit_dir,
            records,
            next_sequence,
        })
    }

    /// 验证策略种类后加载记录、删除已过期有效文件，并按策略处理损坏文件。
    /// `now_ms` 由调用方注入，使保留边界可测试且不依赖读取过程中的时钟漂移。
    pub fn from_audit_dir_with_policy(
        audit_dir: impl AsRef<Path>,
        policy: &ObservabilityRetentionPolicy,
        now_ms: u128,
    ) -> Result<(Self, ObservabilityRetentionReport), EvaError> {
        policy.validate()?;
        if policy.sink_kind != ObservabilitySinkPolicyKind::DurableAudit {
            return Err(EvaError::invalid_argument(
                "filesystem audit sink requires durable-audit retention policy",
            )
            .with_context("sink_kind", policy.sink_kind.as_str()));
        }
        let audit_dir = audit_dir.as_ref().to_path_buf();
        fs::create_dir_all(&audit_dir).map_err(|error| {
            EvaError::internal("failed to create audit directory")
                .with_context("path", audit_dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let (records, report) = load_records_with_policy(&audit_dir, policy, now_ms)?;
        let next_sequence = next_sequence_after(&audit_dir, &records)?;
        Ok((
            Self {
                audit_dir,
                records,
                next_sequence,
            },
            report,
        ))
    }

    /// 返回实际审计目录。
    pub fn audit_dir(&self) -> &Path {
        &self.audit_dir
    }

    /// 返回按 sequence 排序的已加载有效记录。
    pub fn records(&self) -> &[AuditRecord] {
        &self.records
    }

    /// 在约定的 trace 标识字段中精确查找值，并克隆返回所有匹配记录。
    /// 不搜索任意业务 field，避免普通文本碰巧等于 ID 时形成误命中。
    pub fn query_by_trace_id(&self, trace_id: &str) -> Vec<AuditRecord> {
        self.records
            .iter()
            .filter(|record| {
                record.trace.iter().any(|(key, value)| {
                    matches!(
                        key.as_str(),
                        "span_id" | "request_id" | "event_id" | "correlation_id" | "causation_id"
                    ) && value == trace_id
                })
            })
            .cloned()
            .collect()
    }

    /// 将单条记录直接写入其最终序号文件。
    /// 当前实现不使用临时文件/rename，因此不承诺单文件崩溃原子性；重开时损坏文件按
    /// strict 或 retention policy 处理，且序号扫描仍避免覆盖该文件。
    fn persist_record(&self, record: &AuditRecord) -> Result<(), EvaError> {
        let path = record_path(&self.audit_dir, record.sequence);
        fs::write(&path, record.to_storage()).map_err(|error| {
            EvaError::internal("failed to write audit record")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })
    }
}

impl AuditSink for FileSystemAuditSink {
    /// 分配序号、持久化后再加入内存索引。
    /// 序号在 I/O 前递增，写失败可能在当前进程留下空洞，但绝不会把未落盘记录加入 records。
    fn record(&mut self, event: AuditEvent) -> Result<(), EvaError> {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        let record = AuditRecord::from_event(sequence, &event);
        self.persist_record(&record)?;
        self.records.push(record);
        Ok(())
    }
}

impl AuditRecord {
    /// 将观察层 AuditEvent 投影为可持久化记录，并固定记录时间与 trace 快照。
    fn from_event(sequence: u64, event: &AuditEvent) -> Self {
        Self {
            sequence,
            recorded_at_ms: system_time_millis(event.recorded_at),
            action: event.action.as_str().to_owned(),
            outcome: event.outcome.as_str().to_owned(),
            message: event.message.clone(),
            trace: event
                .trace
                .entries()
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
            fields: event.fields.clone(),
        }
    }

    /// 序列化为 version=1 的逐行文本格式。
    /// message 空串表示 None，重复 trace/field 行保留插入顺序，特殊分隔符使用百分号编码。
    fn to_storage(&self) -> String {
        let mut lines = vec![
            "version=1".to_owned(),
            format!("sequence={}", self.sequence),
            format!("recorded_at_ms={}", self.recorded_at_ms),
            format!("action={}", encode_field(&self.action)),
            format!("outcome={}", encode_field(&self.outcome)),
            format!(
                "message={}",
                self.message
                    .as_ref()
                    .map(|value| encode_field(value))
                    .unwrap_or_default()
            ),
        ];
        lines.extend(
            self.trace
                .iter()
                .map(|(key, value)| format!("trace={}|{}", encode_field(key), encode_field(value))),
        );
        lines.extend(
            self.fields
                .iter()
                .map(|(key, value)| format!("field={}|{}", encode_field(key), encode_field(value))),
        );
        lines.push(String::new());
        lines.join("\n")
    }

    /// 严格解析 version=1 审计记录。
    /// 未知字段、未知版本、数值/键值对损坏或缺少核心字段均返回 Conflict，表示磁盘记录
    /// 不再可信；message/trace/fields 允许为空。
    fn from_storage(data: &str) -> Result<Self, EvaError> {
        let mut version = None;
        let mut sequence = None;
        let mut recorded_at_ms = None;
        let mut action = None;
        let mut outcome = None;
        let mut message = None;
        let mut trace = Vec::new();
        let mut fields = Vec::new();

        for line in data.lines().filter(|line| !line.trim().is_empty()) {
            if let Some(value) = line.strip_prefix("version=") {
                version = Some(value.to_owned());
            } else if let Some(value) = line.strip_prefix("sequence=") {
                sequence = Some(parse_u64("sequence", value)?);
            } else if let Some(value) = line.strip_prefix("recorded_at_ms=") {
                recorded_at_ms = Some(parse_u128("recorded_at_ms", value)?);
            } else if let Some(value) = line.strip_prefix("action=") {
                action = Some(decode_field(value));
            } else if let Some(value) = line.strip_prefix("outcome=") {
                outcome = Some(decode_field(value));
            } else if let Some(value) = line.strip_prefix("message=") {
                message = decode_optional_field(value);
            } else if let Some(value) = line.strip_prefix("trace=") {
                trace.push(parse_pair("trace", value)?);
            } else if let Some(value) = line.strip_prefix("field=") {
                fields.push(parse_pair("field", value)?);
            } else {
                return Err(EvaError::conflict("audit record has unknown field"));
            }
        }

        if version.as_deref() != Some("1") {
            return Err(EvaError::conflict("audit record version is unsupported"));
        }

        Ok(Self {
            sequence: sequence
                .ok_or_else(|| EvaError::conflict("audit record missing sequence"))?,
            recorded_at_ms: recorded_at_ms
                .ok_or_else(|| EvaError::conflict("audit record missing recorded_at_ms"))?,
            action: action.ok_or_else(|| EvaError::conflict("audit record missing action"))?,
            outcome: outcome.ok_or_else(|| EvaError::conflict("audit record missing outcome"))?,
            message,
            trace,
            fields,
        })
    }
}

/// 严格加载目录中的 `.audit` 文件并按记录内 sequence 排序；非审计扩展名被忽略。
/// 任一目标文件不可读或损坏会使整个 sink 打开失败，避免返回不完整审计视图。
fn load_records(audit_dir: &Path) -> Result<Vec<AuditRecord>, EvaError> {
    let mut records = Vec::new();
    for entry in fs::read_dir(audit_dir).map_err(|error| {
        EvaError::internal("failed to read audit directory")
            .with_context("path", audit_dir.display().to_string())
            .with_context("io_error", error.to_string())
    })? {
        let entry = entry.map_err(|error| {
            EvaError::internal("failed to read audit directory entry")
                .with_context("path", audit_dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("audit") {
            continue;
        }
        let data = fs::read_to_string(&path).map_err(|error| {
            EvaError::internal("failed to read audit record")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        records.push(
            AuditRecord::from_storage(&data)
                .map_err(|error| error.with_context("path", path.display().to_string()))?,
        );
    }
    records.sort_by_key(|record| record.sequence);
    Ok(records)
}

/// 按保留策略加载审计目录并生成清理报告。
///
/// 损坏记录可 fail-fast 或“跳过并报告”，但跳过时不删除原文件以保留取证材料；只有成功
/// 解析且严格早于保留边界的记录才删除。时间加法使用 saturating 避免极值溢出。
fn load_records_with_policy(
    audit_dir: &Path,
    policy: &ObservabilityRetentionPolicy,
    now_ms: u128,
) -> Result<(Vec<AuditRecord>, ObservabilityRetentionReport), EvaError> {
    let mut records = Vec::new();
    let mut report = ObservabilityRetentionReport::new(policy.sink_kind);
    for entry in fs::read_dir(audit_dir).map_err(|error| {
        EvaError::internal("failed to read audit directory")
            .with_context("path", audit_dir.display().to_string())
            .with_context("io_error", error.to_string())
    })? {
        let entry = entry.map_err(|error| {
            EvaError::internal("failed to read audit directory entry")
                .with_context("path", audit_dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("audit") {
            continue;
        }
        let data = fs::read_to_string(&path).map_err(|error| {
            EvaError::internal("failed to read audit record")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let record = match AuditRecord::from_storage(&data) {
            Ok(record) => record,
            Err(error) => match policy.corrupt_record_policy {
                ObservabilityCorruptRecordPolicy::SkipAndReport => {
                    report.record_corrupt_file(path.display().to_string(), 1);
                    continue;
                }
                ObservabilityCorruptRecordPolicy::FailFast => {
                    return Err(error.with_context("path", path.display().to_string()));
                }
            },
        };
        if record
            .recorded_at_ms
            .saturating_add(policy.retain_for_ms as u128)
            < now_ms
        {
            fs::remove_file(&path).map_err(|error| {
                EvaError::internal("failed to delete expired audit record")
                    .with_context("path", path.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
            report.deleted_files += 1;
            continue;
        }
        report.retained_files += 1;
        records.push(record);
    }
    records.sort_by_key(|record| record.sequence);
    Ok((records, report))
}

/// 以 20 位零填充序号生成文件名，使目录字典序与数值顺序一致。
fn record_path(audit_dir: &Path, sequence: u64) -> PathBuf {
    audit_dir.join(format!("{sequence:020}.audit"))
}

/// 计算不会覆盖任何现有 `.audit` 文件的下一序号。
/// 除有效记录外还扫描可解析的文件名，因此被策略跳过的损坏记录仍保留其序号占位。
fn next_sequence_after(audit_dir: &Path, records: &[AuditRecord]) -> Result<u64, EvaError> {
    let mut max_sequence = records
        .iter()
        .map(|record| record.sequence)
        .max()
        .unwrap_or(0);
    for entry in fs::read_dir(audit_dir).map_err(|error| {
        EvaError::internal("failed to read audit directory")
            .with_context("path", audit_dir.display().to_string())
            .with_context("io_error", error.to_string())
    })? {
        let entry = entry.map_err(|error| {
            EvaError::internal("failed to read audit directory entry")
                .with_context("path", audit_dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("audit") {
            continue;
        }
        if let Some(sequence) = path
            .file_stem()
            .and_then(|value| value.to_str())
            .and_then(|value| value.parse::<u64>().ok())
        {
            max_sequence = max_sequence.max(sequence);
        }
    }
    Ok(max_sequence + 1)
}

/// 将系统时间转换为 epoch 毫秒；早于 epoch 时安全回退 0。
fn system_time_millis(value: SystemTime) -> u128 {
    value
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

/// 解析编码后的 `key|value`；分隔符已被转义，因此字段数必须恰为 2。
fn parse_pair(field: &'static str, value: &str) -> Result<(String, String), EvaError> {
    let parts = value.split('|').map(str::to_owned).collect::<Vec<_>>();
    if parts.len() != 2 {
        return Err(EvaError::conflict("audit record pair has invalid arity")
            .with_context("field", field)
            .with_context("actual", parts.len().to_string()));
    }
    Ok((decode_field(&parts[0]), decode_field(&parts[1])))
}

/// 解析审计序号，并以 Conflict 附加字段和值上下文报告损坏数据。
fn parse_u64(field: &'static str, value: &str) -> Result<u64, EvaError> {
    value.parse::<u64>().map_err(|_| {
        EvaError::conflict("audit record number is invalid")
            .with_context("field", field)
            .with_context("value", value)
    })
}

/// 解析 epoch 毫秒，并以 Conflict 报告损坏磁盘数值。
fn parse_u128(field: &'static str, value: &str) -> Result<u128, EvaError> {
    value.parse::<u128>().map_err(|_| {
        EvaError::conflict("audit record number is invalid")
            .with_context("field", field)
            .with_context("value", value)
    })
}

/// 将磁盘空串恢复为 None，非空值经过百分号解码。
fn decode_optional_field(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(decode_field(value))
    }
}

/// 百分号编码会破坏逐行/键值对语法的字符；先编码 `%` 保证解码无二义性。
fn encode_field(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\n', "%0A")
        .replace('\r', "%0D")
        .replace('\t', "%09")
        .replace('|', "%7C")
        .replace('=', "%3D")
}

/// 按与编码相反的顺序恢复特殊字符，最后解码 `%25` 防止二次展开。
fn decode_field(value: &str) -> String {
    value
        .replace("%0A", "\n")
        .replace("%0D", "\r")
        .replace("%09", "\t")
        .replace("%7C", "|")
        .replace("%3D", "=")
        .replace("%25", "%")
}

#[cfg(test)]
/// 审计持久化、trace 查询、损坏记录策略、保留删除和序号防覆盖回归测试。
mod tests {
    use super::*;
    use eva_observability::{AuditAction, AuditOutcome, SpanId, TraceFields};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    /// 验证审计记录重开后保持字段，并可按 span ID 查询且文件名零填充。
    fn filesystem_audit_sink_round_trips_and_queries_trace() {
        let root = test_root("round-trip");
        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let trace = TraceFields::default()
            .with_request_id(eva_core::RequestId::parse("req-audit-1").unwrap())
            .with_span_id(SpanId::parse("span-audit-1").unwrap());
        let event = AuditEvent::new(AuditAction::RuntimeStarted, AuditOutcome::Ok, trace)
            .with_message("runtime started")
            .with_field("generation", "basic-v1.0");
        let mut sink = FileSystemAuditSink::open(backend.layout()).unwrap();

        sink.record(event).unwrap();
        let reopened = FileSystemAuditSink::open(backend.layout()).unwrap();
        let records = reopened.query_by_trace_id("span-audit-1");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].sequence, 1);
        assert_eq!(records[0].action, "runtime.started");
        assert_eq!(records[0].outcome, "ok");
        assert_eq!(records[0].message.as_deref(), Some("runtime started"));
        assert_eq!(
            records[0].fields,
            vec![("generation".to_owned(), "basic-v1.0".to_owned())]
        );
        assert!(backend
            .layout()
            .audit_dir
            .join("00000000000000000001.audit")
            .is_file());
    }

    #[test]
    /// 验证 strict open 遇到缺必填字段的记录返回 Conflict。
    fn filesystem_audit_sink_rejects_corrupt_record() {
        let root = test_root("corrupt");
        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        fs::write(
            backend
                .layout()
                .audit_dir
                .join("00000000000000000001.audit"),
            "version=1\nsequence=1\nrecorded_at_ms=1\naction=runtime.started\n",
        )
        .unwrap();

        let error = FileSystemAuditSink::open(backend.layout()).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    /// 验证策略仅删除过期有效记录、保留损坏取证文件，并从最高文件序号继续写入。
    fn filesystem_audit_policy_skips_corrupt_and_deletes_only_expired_records() {
        let root = test_root("policy");
        let backend = crate::FileSystemDurableBackend::open(
            crate::DurableBackendOptions::read_write(root.path()),
        )
        .unwrap();
        let audit_dir = &backend.layout().audit_dir;
        let old = AuditRecord {
            sequence: 1,
            recorded_at_ms: 1,
            action: "runtime.started".to_owned(),
            outcome: "ok".to_owned(),
            message: None,
            trace: Vec::new(),
            fields: Vec::new(),
        };
        let fresh = AuditRecord {
            sequence: 2,
            recorded_at_ms: 9_000,
            action: "runtime.started".to_owned(),
            outcome: "ok".to_owned(),
            message: None,
            trace: Vec::new(),
            fields: Vec::new(),
        };
        fs::write(
            audit_dir.join("00000000000000000001.audit"),
            old.to_storage(),
        )
        .unwrap();
        fs::write(
            audit_dir.join("00000000000000000002.audit"),
            fresh.to_storage(),
        )
        .unwrap();
        fs::write(
            audit_dir.join("00000000000000000003.audit"),
            "version=1\nsequence=3\n",
        )
        .unwrap();
        fs::write(audit_dir.join("notes.txt"), "not audit").unwrap();

        let policy = ObservabilityRetentionPolicy::durable_audit()
            .with_retain_for_ms(5_000)
            .with_corrupt_record_policy(ObservabilityCorruptRecordPolicy::SkipAndReport);
        let (mut sink, report) =
            FileSystemAuditSink::open_with_policy(backend.layout(), &policy, 10_000).unwrap();

        assert_eq!(report.deleted_files, 1);
        assert_eq!(report.skipped_corrupt_records, 1);
        assert_eq!(sink.records().len(), 1);
        assert_eq!(sink.records()[0].sequence, 2);
        assert!(!audit_dir.join("00000000000000000001.audit").exists());
        assert!(audit_dir.join("00000000000000000002.audit").exists());
        assert!(audit_dir.join("00000000000000000003.audit").exists());
        assert!(audit_dir.join("notes.txt").exists());

        sink.record(AuditEvent::new(
            AuditAction::RuntimeStarted,
            AuditOutcome::Ok,
            TraceFields::default(),
        ))
        .unwrap();
        assert!(
            audit_dir.join("00000000000000000004.audit").exists(),
            "next sequence must not overwrite skipped corrupt record"
        );
    }

    /// 测试专用临时目录所有者。
    struct TestRoot {
        /// 唯一临时路径。
        path: PathBuf,
    }

    impl TestRoot {
        /// 返回临时目录路径。
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestRoot {
        /// 测试结束时尽力清理，不掩盖原断言结果。
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// 用测试名、进程和时间生成并行安全的临时路径。
    fn test_root(name: &str) -> TestRoot {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        TestRoot {
            path: std::env::temp_dir().join(format!(
                "eva-storage-audit-store-{name}-{}-{now}",
                std::process::id()
            )),
        }
    }
}
