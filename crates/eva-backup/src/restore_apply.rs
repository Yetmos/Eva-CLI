//! 恢复应用的验证、暂存变更、锁、策略、健康门禁与回滚边界。
//! Restore apply validation, staged mutation planning, lock, policy, and health gate boundaries.

use crate::archive::digest_bytes;
use crate::manifest_verifier::ManifestVerifier;
use eva_core::EvaError;
use eva_policy::PolicyDecision;
use eva_storage::ArtifactRecord;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

/// 本模块的架构职责：在持久备份工件之上建立恢复应用的证据、互斥和失败回滚边界。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "restore apply validation, lock, policy, and health gate over durable backup artifacts";

/// 在破坏性恢复前创建的当前状态备份证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreRestoreBackupEvidence {
    /// 前置备份在备份命名空间内的工件标识。
    pub backup_artifact_id: String,
    /// 前置备份工件的预期 SHA-256 摘要。
    pub backup_digest: String,
}

/// 恢复来源、前置安全备份及目标文件变更的完整计划。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreApplyPlan {
    /// 用于确认、锁和事务日志关联的稳定计划标识。
    pub plan_id: String,
    /// 作为恢复数据来源的备份工件标识。
    pub backup_artifact_id: String,
    /// 恢复来源工件的预期摘要。
    pub backup_digest: String,
    /// 恢复前当前状态的备份证据；进入应用门禁前必须存在。
    pub pre_restore_backup: Option<PreRestoreBackupEvidence>,
    /// 所有相对变更路径必须受限于此目标根目录。
    pub mutation_target_root: String,
    /// 按声明顺序执行并按逆序回滚的文件变更步骤。
    pub mutation_steps: Vec<RestoreMutationStep>,
}

/// 恢复应用演练的双备份校验和变更预览报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreApplyDryRunReport {
    /// 被验证计划标识。
    pub plan_id: String,
    /// 恢复来源工件的完整存储键。
    pub backup_artifact_key: String,
    /// 计划声明的恢复来源摘要。
    pub expected_digest: String,
    /// 从恢复来源工件确认的实际摘要。
    pub actual_digest: String,
    /// 前置安全备份的完整存储键。
    pub pre_restore_backup_artifact_key: String,
    /// 计划声明的前置备份摘要。
    pub pre_restore_expected_digest: String,
    /// 从前置备份工件确认的实际摘要。
    pub pre_restore_actual_digest: String,
    /// 演练校验状态。
    pub status: String,
    /// 是否允许直接执行变更；演练报告固定为 `false`。
    pub apply_allowed: bool,
    /// 已规范化且带预检摘要的暂存变更计划。
    pub mutation_plan: RestoreStagedMutationPlan,
    /// 双工件校验和变更规划审计记录。
    pub audit: Vec<String>,
}

/// 恢复引擎支持的文件变更操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreMutationOperation {
    /// 目标必须不存在，随后从工件复制新文件。
    Copy,
    /// 目标必须匹配恢复前摘要，随后删除文件。
    Delete,
    /// 目标必须匹配恢复前摘要，随后用工件内容替换。
    Replace,
}

/// 恢复变更允许的目标节点类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreMutationTargetKind {
    /// 普通文件；符号链接和其他节点类别均被拒绝。
    File,
}

/// 一个经过字段组合校验的恢复文件变更步骤。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreMutationStep {
    /// 要执行的复制、删除或替换操作。
    pub operation: RestoreMutationOperation,
    /// 相对于目标根目录的正斜杠路径。
    pub relative_path: String,
    /// 复制或替换时读取的源工件键。
    pub source_artifact_key: Option<String>,
    /// 复制或替换后内容必须匹配的摘要。
    pub expected_digest: Option<String>,
    /// 删除或替换前现有目标必须匹配的摘要。
    pub pre_restore_digest: Option<String>,
    /// 目标节点类别，当前只能是普通文件。
    pub target_kind: RestoreMutationTargetKind,
}

/// 暂存计划中每一步对应的逆向恢复说明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreRollbackEntry {
    /// 需要回退的相对路径。
    pub relative_path: String,
    /// 删除新复制文件或恢复旧内容的动作名。
    pub action: String,
    /// 需要从前置备份恢复时的旧内容摘要。
    pub pre_restore_digest: Option<String>,
}

/// 可审计、可复现但尚未执行的文件变更计划。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreStagedMutationPlan {
    /// 原始恢复计划标识。
    pub plan_id: String,
    /// 经语法校验的目标根目录。
    pub target_root: String,
    /// 是否声明至少一个文件变更步骤。
    pub mutation_planned: bool,
    /// 是否已经执行文件变更；暂存阶段固定为 `false`。
    pub mutation_executed: bool,
    /// 保留声明顺序的已校验变更步骤。
    pub steps: Vec<RestoreMutationStep>,
    /// 排序去重后的受影响相对路径。
    pub affected_paths: Vec<String>,
    /// 面向审核者的逐步骤预览。
    pub preview: Vec<String>,
    /// 绑定计划、目标、步骤和回滚清单的确定性摘要。
    pub preflight_hash: String,
    /// 每一步的逆向恢复要求。
    pub rollback_manifest: Vec<RestoreRollbackEntry>,
    /// 暂存和预检过程的审计记录。
    pub audit: Vec<String>,
}

/// 事务日志中一个恢复或回滚步骤的状态记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreMutationTransactionEntry {
    /// 步骤在本次正向或逆向执行中的序号。
    pub sequence: usize,
    /// 正向或回滚操作名称。
    pub operation: String,
    /// 受影响的相对路径。
    pub relative_path: String,
    /// `started`、`committed` 或 `failed` 状态。
    pub status: String,
    /// 提交后内容或被删除旧内容的摘要证据。
    pub digest: Option<String>,
    /// 失败时经过行式日志转义的诊断消息。
    pub message: Option<String>,
}

/// 正向恢复文件变更事务的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreMutationApplyReport {
    /// 被执行计划标识。
    pub plan_id: String,
    /// 规范化后的实际目标根目录。
    pub target_root: String,
    /// `applied` 或 `rollback_required` 状态。
    pub status: String,
    /// 是否至少有一个文件系统变更已发生。
    pub mutation_executed: bool,
    /// 是否必须运行回滚引擎恢复一致状态。
    pub rollback_required: bool,
    /// 已成功提交的步骤数量。
    pub completed_steps: usize,
    /// 首个失败步骤的相对路径。
    pub failed_step: Option<String>,
    /// 持久事务日志路径。
    pub transaction_log_path: String,
    /// 本次调用产生的已提交和失败条目。
    pub transaction_log: Vec<RestoreMutationTransactionEntry>,
    /// 事务总体结果的审计记录。
    pub audit: Vec<String>,
}

/// 失败恢复事务的逆向回滚结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreRollbackApplyReport {
    /// 被回滚的恢复计划标识。
    pub plan_id: String,
    /// 规范化后的实际目标根目录。
    pub target_root: String,
    /// `rolled_back` 或 `rollback_failed` 状态。
    pub status: String,
    /// 是否至少执行了一个逆向文件变更。
    pub rollback_executed: bool,
    /// 已成功提交的逆向步骤数量。
    pub completed_steps: usize,
    /// 首个失败逆向步骤的相对路径。
    pub failed_step: Option<String>,
    /// 正向恢复事务日志路径。
    pub transaction_log_path: String,
    /// 独立回滚事务日志路径。
    pub rollback_log_path: String,
    /// 被回滚正向事务的最终状态。
    pub transaction_status: String,
    /// 从正向日志解析出的事务条目。
    pub transaction_log: Vec<RestoreMutationTransactionEntry>,
    /// 本次逆向执行产生的条目。
    pub rollback_log: Vec<RestoreMutationTransactionEntry>,
    /// 回滚结果和人工恢复要求的审计记录。
    pub audit: Vec<String>,
}

/// 从前置备份归档解析出的单个旧文件内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePreRestoreArchiveEntry {
    /// 文件在恢复目标根目录下的相对路径。
    pub relative_path: String,
    /// 恢复操作开始前保存的原始字节。
    pub bytes: Vec<u8>,
    /// 从原始字节重新计算的摘要。
    pub digest: String,
    /// 备份清单中的敏感或脱敏标记。
    pub redacted: bool,
}

/// 经摘要和格式验证的前置备份归档视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePreRestoreArchive {
    /// 原始备份工件键。
    pub artifact_key: String,
    /// 调用方提供的预期工件摘要。
    pub expected_digest: String,
    /// 从工件记录和字节确认的实际摘要。
    pub actual_digest: String,
    /// 按相对路径索引的恢复前文件内容。
    pub entries: BTreeMap<String, RestorePreRestoreArchiveEntry>,
    /// 归档验证和解析审计记录。
    pub audit: Vec<String>,
}

/// 与恢复计划绑定的排他应用锁。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreApplyLock {
    /// 由计划标识派生的锁标识。
    pub lock_id: String,
    /// 被锁定的恢复计划标识。
    pub plan_id: String,
    /// 获取锁的稳定主体标识。
    pub owner: String,
    /// 当前锁状态。
    pub status: String,
    /// 锁获取及用途审计记录。
    pub audit: Vec<String>,
}

/// 进入破坏性恢复前的健康门禁结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreApplyHealthCheck {
    /// 当前环境是否允许继续恢复。
    pub healthy: bool,
    /// 健康说明或失败原因。
    pub message: String,
}

/// 证据、策略、锁和健康检查完成后的恢复应用门禁报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreApplyReport {
    /// 被门禁的恢复计划标识。
    pub plan_id: String,
    /// `gated` 或 `blocked` 状态。
    pub status: String,
    /// 是否允许后续显式执行文件变更。
    pub apply_allowed: bool,
    /// 本协调器是否执行了文件变更；固定为 `false`。
    pub mutation_executed: bool,
    /// 演练阶段生成的不可变暂存计划。
    pub mutation_plan: RestoreStagedMutationPlan,
    /// 已获取的应用锁证据。
    pub lock: RestoreApplyLock,
    /// 健康门禁结果。
    pub health: RestoreApplyHealthCheck,
    /// 已验证恢复来源工件键。
    pub backup_artifact_key: String,
    /// 已验证前置备份工件键。
    pub pre_restore_backup_artifact_key: String,
    /// 已完成及后续必须完成的步骤。
    pub steps: Vec<String>,
    /// 独立发布指针和交接门禁等剩余风险。
    pub risks: Vec<String>,
    /// 证据、策略、锁和健康门禁审计记录。
    pub audit: Vec<String>,
}

/// 恢复应用锁存储的最小接口。
pub trait RestoreApplyLockStore {
    /// 以排他语义为恢复计划获取应用锁。
    fn acquire_lock(
        &mut self,
        plan: &RestoreApplyPlan,
        owner: &str,
    ) -> Result<RestoreApplyLock, EvaError>;
}

/// 单进程协调和测试使用的内存恢复锁存储。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryRestoreApplyLockStore {
    /// 按计划标识保存且不会自动释放的锁。
    locks: BTreeMap<String, RestoreApplyLock>,
}

/// 通过排他创建文件实现跨进程恢复互斥的存储。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemRestoreApplyLockStore {
    /// 应用锁和回滚锁的共同根目录。
    root: PathBuf,
}

/// 验证双备份证据并生成演练报告的无状态服务。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RestoreApplyValidator;

/// 聚合策略、锁和健康检查但不执行文件变更的门禁协调器。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RestoreApplyCoordinator;

/// 规范化变更步骤并生成预检摘要和回滚清单的规划器。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RestoreStagedMutationPlanner;

/// 按事务日志执行暂存文件变更的正向引擎。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RestoreMutationEngine;

/// 根据正向日志和前置备份逆序恢复文件的回滚引擎。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RestoreRollbackEngine;

impl RestoreApplyPlan {
    /// 校验稳定标识和来源摘要后创建尚无前置证据及变更步骤的计划。
    pub fn new(
        plan_id: impl Into<String>,
        backup_artifact_id: impl Into<String>,
        backup_digest: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let plan_id = validate_token("plan_id", plan_id.into())?;
        let backup_artifact_id = validate_token("backup_artifact_id", backup_artifact_id.into())?;
        let backup_digest = backup_digest.into();
        if !backup_digest.starts_with("sha256:") {
            return Err(EvaError::invalid_argument(
                "restore apply plan backup digest must be sha256",
            )
            .with_context("plan_id", &plan_id)
            .with_context("backup_digest", backup_digest));
        }
        Ok(Self {
            plan_id,
            backup_artifact_id,
            backup_digest,
            pre_restore_backup: None,
            mutation_target_root: ".".to_owned(),
            mutation_steps: Vec::new(),
        })
    }

    /// 返回恢复来源在备份命名空间中的完整工件键。
    pub fn backup_artifact_key(&self) -> String {
        format!("backup/{}", self.backup_artifact_id)
    }

    /// 返回由计划标识派生的应用锁标识。
    pub fn lock_id(&self) -> String {
        format!("restore-apply-{}", self.plan_id)
    }

    /// 附加恢复前当前状态的安全备份证据。
    pub fn with_pre_restore_backup(mut self, evidence: PreRestoreBackupEvidence) -> Self {
        self.pre_restore_backup = Some(evidence);
        self
    }

    /// 校验并设置所有文件变更受限的目标根目录。
    pub fn with_mutation_target_root(
        mut self,
        target_root: impl Into<String>,
    ) -> Result<Self, EvaError> {
        self.mutation_target_root = validate_restore_target_root(target_root.into())?;
        Ok(self)
    }

    /// 设置已经由构造器完成字段组合校验的变更步骤。
    pub fn with_mutation_steps(mut self, steps: Vec<RestoreMutationStep>) -> Self {
        self.mutation_steps = steps;
        self
    }
}

impl PreRestoreBackupEvidence {
    /// 校验备份标识和 SHA-256 摘要后创建前置安全证据。
    pub fn new(
        backup_artifact_id: impl Into<String>,
        backup_digest: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let backup_artifact_id =
            validate_token("pre_restore_backup_artifact_id", backup_artifact_id.into())?;
        let backup_digest = backup_digest.into();
        if !backup_digest.starts_with("sha256:") {
            return Err(
                EvaError::invalid_argument("pre-restore backup digest must be sha256")
                    .with_context("backup_artifact_id", &backup_artifact_id)
                    .with_context("backup_digest", backup_digest),
            );
        }
        Ok(Self {
            backup_artifact_id,
            backup_digest,
        })
    }

    /// 返回前置备份在备份命名空间中的完整工件键。
    pub fn backup_artifact_key(&self) -> String {
        format!("backup/{}", self.backup_artifact_id)
    }
}

impl RestoreApplyHealthCheck {
    /// 构造允许继续进入应用门禁的健康结果。
    pub fn healthy() -> Self {
        Self {
            healthy: true,
            message: "healthy".to_owned(),
        }
    }

    /// 构造带非空原因的失败健康结果。
    pub fn failed(message: impl Into<String>) -> Result<Self, EvaError> {
        let message = message.into();
        if message.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "restore apply health failure message is required",
            ));
        }
        Ok(Self {
            healthy: false,
            message,
        })
    }
}

impl RestoreMutationOperation {
    /// 返回写入预检摘要和事务日志的稳定操作名。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Delete => "delete",
            Self::Replace => "replace",
        }
    }

    /// 从持久日志或清单字符串解析操作。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "copy" => Ok(Self::Copy),
            "delete" => Ok(Self::Delete),
            "replace" => Ok(Self::Replace),
            _ => Err(EvaError::invalid_argument(
                "restore mutation operation must be copy, delete, or replace",
            )
            .with_context("operation", value)),
        }
    }
}

impl RestoreMutationTargetKind {
    /// 返回写入预检摘要的稳定目标类别名。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
        }
    }

    /// 解析目标类别并显式拒绝符号链接。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "file" => Ok(Self::File),
            "symlink" => Err(EvaError::invalid_argument(
                "restore mutation plan rejects symlink targets",
            )
            .with_context("target_kind", value)),
            _ => Err(
                EvaError::invalid_argument("restore mutation target kind must be file")
                    .with_context("target_kind", value),
            ),
        }
    }
}

impl RestoreMutationStep {
    /// 创建目标必须不存在的文件复制步骤。
    pub fn copy_file(
        relative_path: impl Into<String>,
        source_artifact_key: impl Into<String>,
        expected_digest: impl Into<String>,
    ) -> Result<Self, EvaError> {
        Self::new(
            RestoreMutationOperation::Copy,
            relative_path,
            Some(source_artifact_key.into()),
            Some(expected_digest.into()),
            None,
            RestoreMutationTargetKind::File,
        )
    }

    /// 创建先校验旧摘要再删除文件的步骤。
    pub fn delete_file(
        relative_path: impl Into<String>,
        pre_restore_digest: impl Into<String>,
    ) -> Result<Self, EvaError> {
        Self::new(
            RestoreMutationOperation::Delete,
            relative_path,
            None,
            None,
            Some(pre_restore_digest.into()),
            RestoreMutationTargetKind::File,
        )
    }

    /// 创建先校验旧摘要再以工件内容替换文件的步骤。
    pub fn replace_file(
        relative_path: impl Into<String>,
        source_artifact_key: impl Into<String>,
        expected_digest: impl Into<String>,
        pre_restore_digest: impl Into<String>,
    ) -> Result<Self, EvaError> {
        Self::new(
            RestoreMutationOperation::Replace,
            relative_path,
            Some(source_artifact_key.into()),
            Some(expected_digest.into()),
            Some(pre_restore_digest.into()),
            RestoreMutationTargetKind::File,
        )
    }

    /// 校验路径、摘要和操作特有字段组合后创建通用步骤。
    ///
    /// Copy 必须有来源和新摘要且不能有旧摘要；Delete 只能有旧摘要；Replace 三者
    /// 必须齐全。提前固定这些不变量可让执行引擎在碰触文件系统前失败关闭。
    pub fn new(
        operation: RestoreMutationOperation,
        relative_path: impl Into<String>,
        source_artifact_key: Option<String>,
        expected_digest: Option<String>,
        pre_restore_digest: Option<String>,
        target_kind: RestoreMutationTargetKind,
    ) -> Result<Self, EvaError> {
        let relative_path = validate_restore_relative_path("relative_path", relative_path.into())?;
        let source_artifact_key = match source_artifact_key {
            Some(value) => Some(validate_restore_artifact_key(value)?),
            None => None,
        };
        let expected_digest = match expected_digest {
            Some(value) => Some(validate_restore_digest("expected_digest", value)?),
            None => None,
        };
        let pre_restore_digest = match pre_restore_digest {
            Some(value) => Some(validate_restore_digest("pre_restore_digest", value)?),
            None => None,
        };
        match operation {
            RestoreMutationOperation::Copy => {
                require_some(
                    "source_artifact_key",
                    source_artifact_key.as_deref(),
                    operation,
                    &relative_path,
                )?;
                require_some(
                    "expected_digest",
                    expected_digest.as_deref(),
                    operation,
                    &relative_path,
                )?;
                if pre_restore_digest.is_some() {
                    return Err(EvaError::invalid_argument(
                        "restore copy mutation cannot include a pre-restore digest",
                    )
                    .with_context("relative_path", &relative_path));
                }
            }
            RestoreMutationOperation::Replace => {
                require_some(
                    "source_artifact_key",
                    source_artifact_key.as_deref(),
                    operation,
                    &relative_path,
                )?;
                require_some(
                    "expected_digest",
                    expected_digest.as_deref(),
                    operation,
                    &relative_path,
                )?;
                require_some(
                    "pre_restore_digest",
                    pre_restore_digest.as_deref(),
                    operation,
                    &relative_path,
                )?;
            }
            RestoreMutationOperation::Delete => {
                if source_artifact_key.is_some() || expected_digest.is_some() {
                    return Err(EvaError::invalid_argument(
                        "restore delete mutation cannot include source artifact or expected digest",
                    )
                    .with_context("relative_path", &relative_path));
                }
                require_some(
                    "pre_restore_digest",
                    pre_restore_digest.as_deref(),
                    operation,
                    &relative_path,
                )?;
            }
        }
        Ok(Self {
            operation,
            relative_path,
            source_artifact_key,
            expected_digest,
            pre_restore_digest,
            target_kind,
        })
    }
}

impl InMemoryRestoreApplyLockStore {
    /// 创建空的内存恢复锁存储。
    pub fn new() -> Self {
        Self::default()
    }
}

impl FileSystemRestoreApplyLockStore {
    /// 创建以指定目录为持久化边界的恢复锁存储。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 返回锁文件根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 为失败恢复事务获取与应用锁分离的排他回滚锁。
    ///
    /// 独立后缀允许同一计划在保留应用锁证据的同时进入一次回滚；`create_new` 是
    /// 跨进程竞争的线性化点。载荷写入失败时锁文件仍保留，以失败关闭避免重复回滚。
    pub fn acquire_rollback_lock(
        &mut self,
        plan: &RestoreApplyPlan,
        owner: &str,
    ) -> Result<RestoreApplyLock, EvaError> {
        let mut lock = build_restore_lock(plan, owner)?;
        lock.lock_id = format!("restore-rollback-{}", plan.plan_id);
        lock.audit.push("lock:rollback".to_owned());
        fs::create_dir_all(&self.root).map_err(|error| {
            EvaError::internal("failed to create restore rollback lock store")
                .with_context("lock_store", self.root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let lock_path = restore_rollback_lock_path(&self.root, &plan.plan_id);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    restore_rollback_lock_conflict(&plan.plan_id, &lock_path)
                } else {
                    EvaError::internal("failed to create restore rollback lock")
                        .with_context("plan_id", &plan.plan_id)
                        .with_context("lock_path", lock_path.display().to_string())
                        .with_context("io_error", error.to_string())
                }
            })?;
        file.write_all(restore_lock_payload(plan, &lock).as_bytes())
            .map_err(|error| {
                EvaError::internal("failed to write restore rollback lock")
                    .with_context("plan_id", &plan.plan_id)
                    .with_context("lock_path", lock_path.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
        Ok(lock)
    }
}

impl RestoreApplyLockStore for InMemoryRestoreApplyLockStore {
    /// 若计划尚未持锁则记录锁，否则返回冲突。
    fn acquire_lock(
        &mut self,
        plan: &RestoreApplyPlan,
        owner: &str,
    ) -> Result<RestoreApplyLock, EvaError> {
        if self.locks.contains_key(&plan.plan_id) {
            return Err(restore_lock_conflict(&plan.plan_id, None));
        }
        let lock = build_restore_lock(plan, owner)?;
        self.locks.insert(plan.plan_id.clone(), lock.clone());
        Ok(lock)
    }
}

impl RestoreApplyLockStore for FileSystemRestoreApplyLockStore {
    /// 通过 `create_new` 排他创建应用锁文件，保证并发进程只有一个成功。
    ///
    /// 写入锁载荷失败时不会删除已创建文件，避免状态不明时第二个进程重新进入恢复。
    fn acquire_lock(
        &mut self,
        plan: &RestoreApplyPlan,
        owner: &str,
    ) -> Result<RestoreApplyLock, EvaError> {
        let lock = build_restore_lock(plan, owner)?;
        fs::create_dir_all(&self.root).map_err(|error| {
            EvaError::internal("failed to create restore apply lock store")
                .with_context("lock_store", self.root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let lock_path = restore_lock_path(&self.root, &plan.plan_id);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    restore_lock_conflict(&plan.plan_id, Some(&lock_path))
                } else {
                    EvaError::internal("failed to create restore apply lock")
                        .with_context("plan_id", &plan.plan_id)
                        .with_context("lock_path", lock_path.display().to_string())
                        .with_context("io_error", error.to_string())
                }
            })?;
        file.write_all(restore_lock_payload(plan, &lock).as_bytes())
            .map_err(|error| {
                EvaError::internal("failed to write restore apply lock")
                    .with_context("plan_id", &plan.plan_id)
                    .with_context("lock_path", lock_path.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
        Ok(lock)
    }
}

impl RestoreApplyValidator {
    /// 验证恢复来源与前置安全备份，并生成绝不允许执行的演练报告。
    ///
    /// 两份工件先分别核对完整键，再从实际字节验证摘要；缺少前置备份证据或工件会
    /// 立即失败。只有双证据通过后才规划文件变更和预检摘要。该阶段不获取锁、不做
    /// 策略判断，也不接触目标文件系统，`apply_allowed` 始终为 `false`。
    pub fn dry_run(
        &self,
        plan: &RestoreApplyPlan,
        artifact: &ArtifactRecord,
        pre_restore_artifact: Option<&ArtifactRecord>,
    ) -> Result<RestoreApplyDryRunReport, EvaError> {
        let expected_key = plan.backup_artifact_key();
        if artifact.key != expected_key {
            return Err(
                EvaError::conflict("restore apply backup artifact key mismatch")
                    .with_context("plan_id", &plan.plan_id)
                    .with_context("expected_artifact_key", expected_key)
                    .with_context("actual_artifact_key", &artifact.key),
            );
        }
        let pre_restore = plan.pre_restore_backup.as_ref().ok_or_else(|| {
            EvaError::invalid_argument("restore apply requires pre-restore backup evidence")
                .with_context("plan_id", &plan.plan_id)
                .with_context("required_field", "pre_restore_backup_artifact_id")
                .with_context("required_field", "pre_restore_backup_digest")
        })?;
        let pre_restore_artifact = pre_restore_artifact.ok_or_else(|| {
            EvaError::not_found("restore apply pre-restore backup artifact is missing")
                .with_context("plan_id", &plan.plan_id)
                .with_context("artifact_key", pre_restore.backup_artifact_key())
        })?;
        let expected_pre_restore_key = pre_restore.backup_artifact_key();
        if pre_restore_artifact.key != expected_pre_restore_key {
            return Err(EvaError::conflict(
                "restore apply pre-restore backup artifact key mismatch",
            )
            .with_context("plan_id", &plan.plan_id)
            .with_context("expected_artifact_key", expected_pre_restore_key)
            .with_context("actual_artifact_key", &pre_restore_artifact.key));
        }
        let verification = ManifestVerifier::verify_artifact(artifact, &plan.backup_digest)?;
        let pre_restore_verification =
            ManifestVerifier::verify_artifact(pre_restore_artifact, &pre_restore.backup_digest)?;
        let mutation_plan = RestoreStagedMutationPlanner.plan(plan)?;
        let mut audit = vec![
            "restore.apply:dry_run".to_owned(),
            "backup:verified".to_owned(),
            "pre_restore_backup:verified".to_owned(),
            "apply_allowed:false".to_owned(),
        ];
        audit.extend(
            mutation_plan
                .audit
                .iter()
                .map(|entry| format!("mutation:{entry}")),
        );
        Ok(RestoreApplyDryRunReport {
            plan_id: plan.plan_id.clone(),
            backup_artifact_key: artifact.key.clone(),
            expected_digest: verification.expected_digest,
            actual_digest: verification.actual_digest,
            pre_restore_backup_artifact_key: pre_restore_artifact.key.clone(),
            pre_restore_expected_digest: pre_restore_verification.expected_digest,
            pre_restore_actual_digest: pre_restore_verification.actual_digest,
            status: "dry_run_validated".to_owned(),
            apply_allowed: false,
            mutation_plan,
            audit,
        })
    }
}

impl RestoreStagedMutationPlanner {
    /// 从已校验步骤生成确定性预览、影响路径、回滚清单和预检摘要。
    ///
    /// 受影响路径排序去重，但执行步骤保留声明顺序；预检摘要同时绑定原始顺序与
    /// 规范化路径集合，使后续回滚日志无法被替换为另一份“看似等价”的计划。
    pub fn plan(&self, plan: &RestoreApplyPlan) -> Result<RestoreStagedMutationPlan, EvaError> {
        let target_root = validate_restore_target_root(plan.mutation_target_root.clone())?;
        let mut affected_paths = BTreeSet::new();
        let mut preview = Vec::new();
        let mut rollback_manifest = Vec::new();
        for step in &plan.mutation_steps {
            affected_paths.insert(step.relative_path.clone());
            preview.push(mutation_preview(step));
            rollback_manifest.push(rollback_entry(step));
        }
        let affected_paths = affected_paths.into_iter().collect::<Vec<_>>();
        let preflight_hash = digest_bytes(
            canonical_mutation_plan_payload(
                plan,
                &target_root,
                &affected_paths,
                &rollback_manifest,
            )
            .as_bytes(),
        );
        let mutation_planned = !plan.mutation_steps.is_empty();
        let mut audit = vec!["restore.mutation:plan_only".to_owned()];
        if mutation_planned {
            audit.push("restore.mutation:staged_steps_validated".to_owned());
            audit.push(format!(
                "restore.mutation:affected_paths={}",
                affected_paths.len()
            ));
            audit.push("mutation_executed:false".to_owned());
        } else {
            audit.push("restore.mutation:no_steps_declared".to_owned());
        }
        Ok(RestoreStagedMutationPlan {
            plan_id: plan.plan_id.clone(),
            target_root,
            mutation_planned,
            mutation_executed: false,
            steps: plan.mutation_steps.clone(),
            affected_paths,
            preview,
            preflight_hash,
            rollback_manifest,
            audit,
        })
    }
}

impl RestoreMutationEngine {
    /// 按暂存顺序执行文件变更，并在每一步前后追加事务日志。
    ///
    /// 引擎先创建并规范化目标根目录，然后初始化包含 plan id 和 preflight hash 的
    /// 日志。每步先记录 started，再执行受路径/摘要约束的变更，成功后记录 committed。
    /// 首个失败会停止后续步骤并写入 rollback_required；若失败发生在删除旧文件之后，
    /// `mutation_executed` 仍为真。跨多个步骤不具备整体原子性，事务日志和前置备份
    /// 是唯一回滚依据，调用方必须在报告要求时立即运行回滚引擎。
    pub fn apply(
        &self,
        plan: &RestoreStagedMutationPlan,
        target_root: impl AsRef<Path>,
        transaction_log_path: impl AsRef<Path>,
        source_artifacts: &BTreeMap<String, ArtifactRecord>,
    ) -> Result<RestoreMutationApplyReport, EvaError> {
        let target_root = target_root.as_ref();
        let transaction_log_path = transaction_log_path.as_ref();
        fs::create_dir_all(target_root).map_err(|error| {
            EvaError::internal("failed to create restore mutation target root")
                .with_context("target_root", target_root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let root_canonical = fs::canonicalize(target_root).map_err(|error| {
            EvaError::internal("failed to canonicalize restore mutation target root")
                .with_context("target_root", target_root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        if let Some(parent) = transaction_log_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                EvaError::internal("failed to create restore mutation transaction log directory")
                    .with_context(
                        "transaction_log",
                        transaction_log_path.display().to_string(),
                    )
                    .with_context("io_error", error.to_string())
            })?;
        }
        // 先持久化计划身份与预检摘要，确保任何文件变更都能追溯到确切计划。
        fs::write(
            transaction_log_path,
            restore_transaction_log_header(plan, &root_canonical),
        )
        .map_err(|error| {
            EvaError::internal("failed to initialize restore mutation transaction log")
                .with_context(
                    "transaction_log",
                    transaction_log_path.display().to_string(),
                )
                .with_context("io_error", error.to_string())
        })?;

        let mut transaction_log = Vec::new();
        let mut mutation_executed = false;
        for (sequence, step) in plan.steps.iter().enumerate() {
            // started 记录必须先于文件系统操作，崩溃恢复据此识别未确认步骤。
            append_restore_transaction_log(
                transaction_log_path,
                &RestoreMutationTransactionEntry {
                    sequence,
                    operation: step.operation.as_str().to_owned(),
                    relative_path: step.relative_path.clone(),
                    status: "started".to_owned(),
                    digest: None,
                    message: None,
                },
            )?;
            match apply_restore_mutation_step(sequence, &root_canonical, step, source_artifacts) {
                Ok(entry) => {
                    mutation_executed = true;
                    append_restore_transaction_log(transaction_log_path, &entry)?;
                    transaction_log.push(entry);
                }
                Err(failure) => {
                    mutation_executed |= failure.mutation_executed;
                    let failed_entry = RestoreMutationTransactionEntry {
                        sequence,
                        operation: step.operation.as_str().to_owned(),
                        relative_path: step.relative_path.clone(),
                        status: "failed".to_owned(),
                        digest: None,
                        message: Some(failure.error.to_string()),
                    };
                    append_restore_transaction_log(transaction_log_path, &failed_entry)?;
                    transaction_log.push(failed_entry);
                    // 失败即停止；已提交步骤不会在正向引擎中自动撤销。
                    append_restore_transaction_status(
                        transaction_log_path,
                        "rollback_required",
                        mutation_executed,
                    )?;
                    return Ok(RestoreMutationApplyReport {
                        plan_id: plan.plan_id.clone(),
                        target_root: root_canonical.display().to_string(),
                        status: "rollback_required".to_owned(),
                        mutation_executed,
                        rollback_required: true,
                        completed_steps: transaction_log
                            .iter()
                            .filter(|entry| entry.status == "committed")
                            .count(),
                        failed_step: Some(step.relative_path.clone()),
                        transaction_log_path: transaction_log_path.display().to_string(),
                        transaction_log,
                        audit: vec![
                            "restore.mutation:transaction_failed".to_owned(),
                            "restore.mutation:rollback_required".to_owned(),
                        ],
                    });
                }
            }
        }
        append_restore_transaction_status(transaction_log_path, "applied", mutation_executed)?;
        Ok(RestoreMutationApplyReport {
            plan_id: plan.plan_id.clone(),
            target_root: root_canonical.display().to_string(),
            status: "applied".to_owned(),
            mutation_executed,
            rollback_required: false,
            completed_steps: transaction_log.len(),
            failed_step: None,
            transaction_log_path: transaction_log_path.display().to_string(),
            transaction_log,
            audit: vec![
                "restore.mutation:transaction_applied".to_owned(),
                "mutation_executed:true".to_owned(),
            ],
        })
    }
}

impl RestoreRollbackEngine {
    /// 验证失败事务身份后，按已执行步骤的逆序恢复前置备份内容。
    ///
    /// 仅接受 plan id、preflight hash 均匹配且状态为 rollback_required 的日志。回滚
    /// 候选按正向日志逆序生成；执行前还会验证当前文件仍是正向恢复产生的内容，
    /// 防止覆盖故障后其他进程的新写入。删除/替换的旧内容必须来自摘要匹配的前置
    /// 归档。任一步回滚失败即停止并返回 manual recovery required，不假装整体原子。
    pub fn apply(
        &self,
        plan: &RestoreStagedMutationPlan,
        target_root: impl AsRef<Path>,
        transaction_log_path: impl AsRef<Path>,
        rollback_log_path: impl AsRef<Path>,
        pre_restore_archive: &RestorePreRestoreArchive,
    ) -> Result<RestoreRollbackApplyReport, EvaError> {
        let target_root = target_root.as_ref();
        let transaction_log_path = transaction_log_path.as_ref();
        let rollback_log_path = rollback_log_path.as_ref();
        let transaction = parse_restore_transaction_log(transaction_log_path)?;
        // 事务身份、计划摘要和失败状态必须全部吻合，不能用任意日志驱动文件恢复。
        if transaction.plan_id != plan.plan_id {
            return Err(
                EvaError::conflict("restore rollback transaction plan mismatch")
                    .with_context("plan_id", &plan.plan_id)
                    .with_context("transaction_plan_id", transaction.plan_id),
            );
        }
        if transaction.preflight_hash != plan.preflight_hash {
            return Err(
                EvaError::conflict("restore rollback preflight hash mismatch")
                    .with_context("plan_id", &plan.plan_id)
                    .with_context("expected_preflight_hash", &plan.preflight_hash)
                    .with_context("transaction_preflight_hash", transaction.preflight_hash),
            );
        }
        if transaction.status != "rollback_required" {
            return Err(EvaError::conflict(
                "restore rollback requires a rollback_required transaction log",
            )
            .with_context("plan_id", &plan.plan_id)
            .with_context("transaction_status", &transaction.status));
        }
        if !transaction.mutation_executed {
            return Err(EvaError::conflict(
                "restore rollback transaction has no executed mutation",
            )
            .with_context("plan_id", &plan.plan_id));
        }

        let root_canonical = fs::canonicalize(target_root).map_err(|error| {
            EvaError::internal("failed to canonicalize restore rollback target root")
                .with_context("target_root", target_root.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        if let Some(parent) = rollback_log_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                EvaError::internal("failed to create restore rollback log directory")
                    .with_context("rollback_log", rollback_log_path.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
        }
        // 独立回滚日志保留逆向执行证据，不覆盖原始正向事务日志。
        fs::write(
            rollback_log_path,
            format!(
                "restore-rollback-transaction:v1\nplan_id={}\ntarget_root={}\npreflight_hash={}\nsource_transaction={}\n",
                plan.plan_id,
                root_canonical.display(),
                plan.preflight_hash,
                transaction_log_path.display()
            ),
        )
        .map_err(|error| {
            EvaError::internal("failed to initialize restore rollback log")
                .with_context("rollback_log", rollback_log_path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;

        let rollback_candidates = rollback_candidates(plan, &transaction)?;
        let mut rollback_log = Vec::new();
        let mut rollback_executed = false;
        for (index, candidate) in rollback_candidates.iter().enumerate() {
            let step = &candidate.step;
            // 与正向事务相同，每个逆向操作先记录 started，再触碰目标文件。
            append_restore_transaction_log(
                rollback_log_path,
                &RestoreMutationTransactionEntry {
                    sequence: index,
                    operation: rollback_operation_name(step).to_owned(),
                    relative_path: step.relative_path.clone(),
                    status: "started".to_owned(),
                    digest: None,
                    message: None,
                },
            )?;
            match apply_restore_rollback_step(
                index,
                &root_canonical,
                candidate,
                pre_restore_archive,
            ) {
                Ok(entry) => {
                    rollback_executed = true;
                    append_restore_transaction_log(rollback_log_path, &entry)?;
                    rollback_log.push(entry);
                }
                Err(error) => {
                    let failed_entry = RestoreMutationTransactionEntry {
                        sequence: index,
                        operation: rollback_operation_name(step).to_owned(),
                        relative_path: step.relative_path.clone(),
                        status: "failed".to_owned(),
                        digest: None,
                        message: Some(error.to_string()),
                    };
                    append_restore_transaction_log(rollback_log_path, &failed_entry)?;
                    rollback_log.push(failed_entry);
                    append_restore_transaction_status(
                        rollback_log_path,
                        "rollback_failed",
                        rollback_executed,
                    )?;
                    return Ok(RestoreRollbackApplyReport {
                        plan_id: plan.plan_id.clone(),
                        target_root: root_canonical.display().to_string(),
                        status: "rollback_failed".to_owned(),
                        rollback_executed,
                        completed_steps: rollback_log
                            .iter()
                            .filter(|entry| entry.status == "committed")
                            .count(),
                        failed_step: Some(step.relative_path.clone()),
                        transaction_log_path: transaction_log_path.display().to_string(),
                        rollback_log_path: rollback_log_path.display().to_string(),
                        transaction_status: transaction.status,
                        transaction_log: transaction.entries,
                        rollback_log,
                        audit: vec![
                            "restore.rollback:transaction_failed".to_owned(),
                            "restore.rollback:manual_recovery_required".to_owned(),
                        ],
                    });
                }
            }
        }
        append_restore_transaction_status(rollback_log_path, "rolled_back", rollback_executed)?;
        Ok(RestoreRollbackApplyReport {
            plan_id: plan.plan_id.clone(),
            target_root: root_canonical.display().to_string(),
            status: "rolled_back".to_owned(),
            rollback_executed,
            completed_steps: rollback_log.len(),
            failed_step: None,
            transaction_log_path: transaction_log_path.display().to_string(),
            rollback_log_path: rollback_log_path.display().to_string(),
            transaction_status: transaction.status,
            transaction_log: transaction.entries,
            rollback_log,
            audit: vec![
                "restore.rollback:transaction_rolled_back".to_owned(),
                "rollback_executed:true".to_owned(),
            ],
        })
    }
}

impl RestoreApplyCoordinator {
    /// 在策略放行后获取锁，并根据健康结果给出最终执行门禁。
    ///
    /// 调用方必须传入先前演练报告；本方法自身不重新读取工件，也不执行暂存变更。
    /// 策略检查先于锁获取，避免被拒请求占用持久锁；锁成功后健康失败会保留锁并
    /// 返回 blocked，采用失败关闭语义阻止并发重试越过已记录的失败状态。只有健康
    /// 通过才令 `apply_allowed` 为真，实际文件执行仍需显式调用变更引擎。
    pub fn apply<S: RestoreApplyLockStore>(
        &self,
        store: &mut S,
        plan: &RestoreApplyPlan,
        dry_run: &RestoreApplyDryRunReport,
        policy_decision: &PolicyDecision,
        health: RestoreApplyHealthCheck,
        owner: &str,
    ) -> Result<RestoreApplyReport, EvaError> {
        // 高风险策略拒绝必须发生在任何持久锁状态写入之前。
        policy_decision.ensure_allowed()?;
        let lock = store.acquire_lock(plan, owner)?;
        let mut audit = vec![
            "restore.apply:plan_parsed".to_owned(),
            "restore.apply:confirmation_matched".to_owned(),
            "restore.apply:backup_evidence_verified".to_owned(),
            "restore.apply:policy_allowed".to_owned(),
            "restore.apply:lock_acquired".to_owned(),
        ];
        audit.extend(
            policy_decision
                .audit
                .iter()
                .map(|entry| format!("policy:{entry}")),
        );
        if health.healthy {
            audit.push("restore.apply:health_check_passed".to_owned());
            audit.push("restore.apply:gated".to_owned());
            Ok(RestoreApplyReport {
                plan_id: plan.plan_id.clone(),
                status: "gated".to_owned(),
                apply_allowed: true,
                mutation_executed: false,
                mutation_plan: dry_run.mutation_plan.clone(),
                lock,
                health,
                backup_artifact_key: dry_run.backup_artifact_key.clone(),
                pre_restore_backup_artifact_key: dry_run.pre_restore_backup_artifact_key.clone(),
                steps: restore_apply_steps(true),
                risks: vec![
                    "destructive restore remains bound to explicit apply gate evidence".to_owned(),
                    "release pointer mutation and supervisor handoff remain separate gates"
                        .to_owned(),
                ],
                audit,
            })
        } else {
            audit.push("restore.apply:health_check_failed".to_owned());
            audit.push("restore.apply:rollback_required".to_owned());
            Ok(RestoreApplyReport {
                plan_id: plan.plan_id.clone(),
                status: "blocked".to_owned(),
                apply_allowed: false,
                mutation_executed: false,
                mutation_plan: dry_run.mutation_plan.clone(),
                lock,
                health,
                backup_artifact_key: dry_run.backup_artifact_key.clone(),
                pre_restore_backup_artifact_key: dry_run.pre_restore_backup_artifact_key.clone(),
                steps: restore_apply_steps(false),
                risks: vec![
                    "restore apply health check failed before destructive mutation".to_owned(),
                    "rollback plan must be emitted before retrying apply".to_owned(),
                ],
                audit,
            })
        }
    }
}

/// 校验会参与锁文件名和事务关联的稳定标识。
fn validate_token(field: &'static str, value: String) -> Result<String, EvaError> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(EvaError::invalid_argument(
            "restore apply plan token must be non-empty and trimmed",
        )
        .with_context("field", field)
        .with_context("value", value));
    }
    if value.contains('/') || value.contains('\\') || value.contains("..") {
        return Err(
            EvaError::invalid_argument("restore apply plan token must be a stable slug")
                .with_context("field", field)
                .with_context("value", value),
        );
    }
    Ok(value)
}

/// 校验恢复目标根目录文本非空、已裁剪且不含遍历或空字节。
///
/// 执行阶段还会对实际目录做 canonicalize 并逐段拒绝符号链接；本函数只负责计划
/// 载荷的可移植语法边界。
fn validate_restore_target_root(value: String) -> Result<String, EvaError> {
    if value.trim().is_empty() || value.trim() != value || value.contains('\0') {
        return Err(EvaError::invalid_argument(
            "restore mutation target root must be non-empty and trimmed",
        )
        .with_context("target_root", value));
    }
    if value.contains("..") {
        return Err(EvaError::invalid_argument(
            "restore mutation target root cannot contain parent traversal",
        )
        .with_context("target_root", value));
    }
    Ok(value)
}

/// 校验恢复路径是只含普通组件的正斜杠相对路径。
///
/// 拒绝盘符、根目录、`.`、`..`、空组件、反斜杠和平台分隔字符，确保拼接后不会
/// 直接逃逸目标根。执行阶段仍须逐段检查符号链接以抵御间接逃逸。
fn validate_restore_relative_path(field: &'static str, value: String) -> Result<String, EvaError> {
    if value.trim().is_empty() || value.trim() != value || value.contains('\0') {
        return Err(EvaError::invalid_argument(
            "restore mutation path must be non-empty and trimmed",
        )
        .with_context("field", field)
        .with_context("path", value));
    }
    if value.contains('\\') || value.contains(':') || value.contains('|') {
        return Err(EvaError::invalid_argument(
            "restore mutation path must be a stable forward-slash relative path",
        )
        .with_context("field", field)
        .with_context("path", value));
    }
    if value.split('/').any(|segment| segment.is_empty()) {
        return Err(EvaError::invalid_argument(
            "restore mutation path cannot contain empty components",
        )
        .with_context("field", field)
        .with_context("path", value));
    }
    for component in Path::new(&value).components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(EvaError::invalid_argument(
                    "restore mutation path must stay inside target root",
                )
                .with_context("field", field)
                .with_context("path", value));
            }
        }
    }
    Ok(value)
}

/// 复用恢复相对路径约束校验源工件键。
fn validate_restore_artifact_key(value: String) -> Result<String, EvaError> {
    validate_restore_relative_path("source_artifact_key", value)
}

/// 校验恢复摘要使用已裁剪的 SHA-256 格式前缀。
fn validate_restore_digest(field: &'static str, value: String) -> Result<String, EvaError> {
    if !value.starts_with("sha256:") || value.trim() != value {
        return Err(
            EvaError::invalid_argument("restore mutation digest must be sha256")
                .with_context("field", field)
                .with_context("digest", value),
        );
    }
    Ok(value)
}

/// 为操作缺少必要可选字段时生成包含步骤上下文的错误。
fn require_some(
    field: &'static str,
    value: Option<&str>,
    operation: RestoreMutationOperation,
    relative_path: &str,
) -> Result<(), EvaError> {
    if value.is_some() {
        return Ok(());
    }
    Err(
        EvaError::invalid_argument("restore mutation step is missing a required field")
            .with_context("field", field)
            .with_context("operation", operation.as_str())
            .with_context("relative_path", relative_path),
    )
}

/// 将一个变更步骤格式化为不执行操作的审核预览。
fn mutation_preview(step: &RestoreMutationStep) -> String {
    match step.operation {
        RestoreMutationOperation::Copy => format!(
            "copy {} from {} expecting {}",
            step.relative_path,
            step.source_artifact_key.as_deref().unwrap_or("<missing>"),
            step.expected_digest.as_deref().unwrap_or("<missing>")
        ),
        RestoreMutationOperation::Delete => format!(
            "delete {} after verifying pre-restore {}",
            step.relative_path,
            step.pre_restore_digest.as_deref().unwrap_or("<missing>")
        ),
        RestoreMutationOperation::Replace => format!(
            "replace {} from {} expecting {} after pre-restore {}",
            step.relative_path,
            step.source_artifact_key.as_deref().unwrap_or("<missing>"),
            step.expected_digest.as_deref().unwrap_or("<missing>"),
            step.pre_restore_digest.as_deref().unwrap_or("<missing>")
        ),
    }
}

/// 从正向变更推导对应的逆向恢复动作。
fn rollback_entry(step: &RestoreMutationStep) -> RestoreRollbackEntry {
    match step.operation {
        RestoreMutationOperation::Copy => RestoreRollbackEntry {
            relative_path: step.relative_path.clone(),
            action: "delete_restored_path".to_owned(),
            pre_restore_digest: None,
        },
        RestoreMutationOperation::Delete | RestoreMutationOperation::Replace => {
            RestoreRollbackEntry {
                relative_path: step.relative_path.clone(),
                action: "restore_pre_restore_digest".to_owned(),
                pre_restore_digest: step.pre_restore_digest.clone(),
            }
        }
    }
}

/// 以固定顺序编码计划、路径集合和回滚清单，供预检摘要绑定。
fn canonical_mutation_plan_payload(
    plan: &RestoreApplyPlan,
    target_root: &str,
    affected_paths: &[String],
    rollback_manifest: &[RestoreRollbackEntry],
) -> String {
    let mut payload = format!(
        "restore-mutation-plan:v1\nplan_id={}\ntarget_root={}\nbackup_artifact_key={}\npre_restore_backup_artifact_key={}\n",
        plan.plan_id,
        target_root,
        plan.backup_artifact_key(),
        plan.pre_restore_backup
            .as_ref()
            .map(PreRestoreBackupEvidence::backup_artifact_key)
            .unwrap_or_else(|| "<missing>".to_owned())
    );
    payload.push_str(&format!("steps={}\n", plan.mutation_steps.len()));
    for (index, step) in plan.mutation_steps.iter().enumerate() {
        payload.push_str(&format!(
            "step[{index}]={}|{}|{}|{}|{}|{}\n",
            step.operation.as_str(),
            step.relative_path,
            step.source_artifact_key.as_deref().unwrap_or("none"),
            step.expected_digest.as_deref().unwrap_or("none"),
            step.pre_restore_digest.as_deref().unwrap_or("none"),
            step.target_kind.as_str()
        ));
    }
    payload.push_str(&format!("affected_paths={}\n", affected_paths.len()));
    for path in affected_paths {
        payload.push_str(&format!("affected={path}\n"));
    }
    payload.push_str(&format!("rollback_entries={}\n", rollback_manifest.len()));
    for (index, entry) in rollback_manifest.iter().enumerate() {
        payload.push_str(&format!(
            "rollback[{index}]={}|{}|{}\n",
            entry.relative_path,
            entry.action,
            entry.pre_restore_digest.as_deref().unwrap_or("none")
        ));
    }
    payload.push_str("mutation_executed=false\n");
    payload
}

/// 区分“步骤失败且未变更”与“替换已删旧文件后失败”的内部错误。
#[derive(Debug)]
struct RestoreMutationStepFailure {
    /// 保留完整上下文的实际错误。
    error: EvaError,
    /// 失败前是否已经发生需要回滚的文件系统变更。
    mutation_executed: bool,
}

/// 生成绑定计划、规范化根目录和预检摘要的事务日志头。
fn restore_transaction_log_header(plan: &RestoreStagedMutationPlan, root: &Path) -> String {
    format!(
        "restore-mutation-transaction:v1\nplan_id={}\ntarget_root={}\npreflight_hash={}\n",
        plan.plan_id,
        root.display(),
        plan.preflight_hash
    )
}

/// 执行一个已校验恢复步骤并返回 committed 日志条目。
///
/// Copy 拒绝覆盖现有目标；Delete 和 Replace 在变更前验证旧摘要；Copy 和 Replace
/// 还会验证源工件记录摘要及计划摘要。Replace 传播删除旧目标后提交新文件失败的
/// 特殊状态，使上层即使没有 committed 条目也能要求回滚。
fn apply_restore_mutation_step(
    sequence: usize,
    root: &Path,
    step: &RestoreMutationStep,
    source_artifacts: &BTreeMap<String, ArtifactRecord>,
) -> Result<RestoreMutationTransactionEntry, RestoreMutationStepFailure> {
    let target_path = checked_restore_target_path(root, &step.relative_path).map_err(step_error)?;
    match step.operation {
        RestoreMutationOperation::Copy => {
            if target_path.exists() {
                return Err(step_error(
                    EvaError::conflict("restore copy target already exists")
                        .with_context("relative_path", &step.relative_path),
                ));
            }
            let source =
                checked_restore_source_artifact(step, source_artifacts).map_err(step_error)?;
            write_restore_bytes_atomically(&target_path, &step.relative_path, sequence, source)
                .map_err(step_error)?;
            Ok(committed_entry(sequence, step, Some(digest_bytes(source))))
        }
        RestoreMutationOperation::Delete => {
            verify_existing_target_digest(&target_path, step).map_err(step_error)?;
            fs::remove_file(&target_path).map_err(|error| {
                step_error(
                    EvaError::internal("failed to delete restore target")
                        .with_context("relative_path", &step.relative_path)
                        .with_context("target_path", target_path.display().to_string())
                        .with_context("io_error", error.to_string()),
                )
            })?;
            Ok(committed_entry(
                sequence,
                step,
                step.pre_restore_digest.clone(),
            ))
        }
        RestoreMutationOperation::Replace => {
            verify_existing_target_digest(&target_path, step).map_err(step_error)?;
            let source =
                checked_restore_source_artifact(step, source_artifacts).map_err(step_error)?;
            replace_restore_target_atomically(&target_path, &step.relative_path, sequence, source)
                .map_err(|failure| RestoreMutationStepFailure {
                    error: failure.error,
                    mutation_executed: failure.mutation_executed,
                })?;
            Ok(committed_entry(sequence, step, Some(digest_bytes(source))))
        }
    }
}

impl RestorePreRestoreArchive {
    /// 验证备份工件摘要并解析恢复前文件内容。
    ///
    /// 解析器只接受 v1 行式归档，逐条校验相对路径、十六进制字节和声明大小；未知
    /// 元数据字段被忽略以支持向前兼容。每个解析条目的摘要均从实际字节重算，供
    /// 回滚时与计划中的 pre_restore_digest 再次绑定。
    pub fn parse(record: &ArtifactRecord, expected_digest: &str) -> Result<Self, EvaError> {
        let verification = ManifestVerifier::verify_artifact(record, expected_digest)?;
        let payload = std::str::from_utf8(&record.bytes).map_err(|error| {
            EvaError::conflict("pre-restore backup archive is not utf-8")
                .with_context("artifact_key", &record.key)
                .with_context("utf8_error", error.to_string())
        })?;
        let mut lines = payload.lines();
        match lines.next() {
            Some("eva-backup-archive:v1") => {}
            _ => {
                return Err(
                    EvaError::unsupported("unsupported pre-restore backup archive format")
                        .with_context("artifact_key", &record.key),
                );
            }
        }
        let mut entries = BTreeMap::new();
        let mut current = PendingArchiveEntry::default();
        for line in lines {
            let Some((key, value)) = line.split_once('=') else {
                return Err(
                    EvaError::conflict("pre-restore archive line must use key=value")
                        .with_context("artifact_key", &record.key)
                        .with_context("line", line),
                );
            };
            match key {
                "entry.path" => {
                    if current.has_any_field() {
                        let entry = std::mem::take(&mut current).finish(&record.key)?;
                        entries.insert(entry.relative_path.clone(), entry);
                    }
                    current.path = Some(value.to_owned());
                }
                "entry.size" => current.size = Some(parse_archive_entry_size(value, &record.key)?),
                "entry.redacted" => {
                    current.redacted = Some(match value {
                        "true" => true,
                        "false" => false,
                        _ => {
                            return Err(EvaError::conflict(
                                "pre-restore archive redacted flag must be boolean",
                            )
                            .with_context("artifact_key", &record.key)
                            .with_context("redacted", value));
                        }
                    })
                }
                "entry.bytes.hex" => current.bytes = Some(hex_decode(value)?),
                _ => {}
            }
        }
        if current.has_any_field() {
            let entry = current.finish(&record.key)?;
            entries.insert(entry.relative_path.clone(), entry);
        }
        let entry_count = entries.len();
        Ok(Self {
            artifact_key: record.key.clone(),
            expected_digest: verification.expected_digest,
            actual_digest: verification.actual_digest,
            entries,
            audit: vec![
                "pre_restore.archive:verified".to_owned(),
                format!("pre_restore.archive:entries={entry_count}"),
            ],
        })
    }

    /// 按相对路径查找恢复前文件内容。
    pub fn entry(&self, relative_path: &str) -> Option<&RestorePreRestoreArchiveEntry> {
        self.entries.get(relative_path)
    }
}

/// 解析行式归档时尚未完成字段校验的临时条目。
#[derive(Debug, Default)]
struct PendingArchiveEntry {
    /// 待校验相对路径。
    path: Option<String>,
    /// 清单声明字节数。
    size: Option<usize>,
    /// 可选敏感或脱敏标记。
    redacted: Option<bool>,
    /// 已解码原始字节。
    bytes: Option<Vec<u8>>,
}

impl PendingArchiveEntry {
    /// 判断是否已经读取当前条目的任一字段。
    fn has_any_field(&self) -> bool {
        self.path.is_some()
            || self.size.is_some()
            || self.redacted.is_some()
            || self.bytes.is_some()
    }

    /// 要求必要字段齐全、校验路径和大小后完成条目。
    fn finish(self, artifact_key: &str) -> Result<RestorePreRestoreArchiveEntry, EvaError> {
        let relative_path = validate_restore_relative_path(
            "pre_restore_archive_entry_path",
            self.path.ok_or_else(|| {
                EvaError::conflict("pre-restore archive entry missing path")
                    .with_context("artifact_key", artifact_key)
            })?,
        )?;
        let bytes = self.bytes.ok_or_else(|| {
            EvaError::conflict("pre-restore archive entry missing bytes")
                .with_context("artifact_key", artifact_key)
                .with_context("relative_path", &relative_path)
        })?;
        let size = self.size.ok_or_else(|| {
            EvaError::conflict("pre-restore archive entry missing size")
                .with_context("artifact_key", artifact_key)
                .with_context("relative_path", &relative_path)
        })?;
        if bytes.len() != size {
            return Err(
                EvaError::conflict("pre-restore archive entry size mismatch")
                    .with_context("artifact_key", artifact_key)
                    .with_context("relative_path", &relative_path)
                    .with_context("expected_size", size.to_string())
                    .with_context("actual_size", bytes.len().to_string()),
            );
        }
        Ok(RestorePreRestoreArchiveEntry {
            relative_path,
            digest: digest_bytes(&bytes),
            bytes,
            redacted: self.redacted.unwrap_or(false),
        })
    }
}

/// 从持久日志解析出的正向恢复事务状态。
struct ParsedRestoreTransactionLog {
    /// 日志声明的计划标识。
    plan_id: String,
    /// 日志声明的暂存计划摘要。
    preflight_hash: String,
    /// 日志最后一次写入的总体状态。
    status: String,
    /// 日志声明是否发生过文件变更。
    mutation_executed: bool,
    /// 按写入顺序解析的步骤状态条目。
    entries: Vec<RestoreMutationTransactionEntry>,
}

/// 解析并验证正向恢复事务日志的必要头字段和步骤格式。
///
/// 同一键重复时保留最后值，从而读取追加写入的最终 status；缺少任何恢复身份或
/// 总体状态字段都会失败，避免以截断日志驱动回滚。
fn parse_restore_transaction_log(path: &Path) -> Result<ParsedRestoreTransactionLog, EvaError> {
    let data = fs::read_to_string(path).map_err(|error| {
        let message = if error.kind() == std::io::ErrorKind::NotFound {
            "restore mutation transaction log is missing"
        } else {
            "failed to read restore mutation transaction log"
        };
        EvaError::not_found(message)
            .with_context("transaction_log", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    let mut plan_id = None;
    let mut preflight_hash = None;
    let mut status = None;
    let mut mutation_executed = None;
    let mut entries = Vec::new();
    for line in data.lines() {
        if line == "restore-mutation-transaction:v1" || line.trim().is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(
                EvaError::conflict("restore transaction log line must use key=value")
                    .with_context("transaction_log", path.display().to_string())
                    .with_context("line", line),
            );
        };
        match key {
            "plan_id" => plan_id = Some(value.to_owned()),
            "preflight_hash" => preflight_hash = Some(value.to_owned()),
            "status" => status = Some(value.to_owned()),
            "mutation_executed" => {
                mutation_executed = Some(match value {
                    "true" => true,
                    "false" => false,
                    _ => {
                        return Err(EvaError::conflict(
                            "restore transaction mutation_executed must be boolean",
                        )
                        .with_context("transaction_log", path.display().to_string())
                        .with_context("mutation_executed", value));
                    }
                })
            }
            "step" => entries.push(parse_restore_transaction_step(value, path)?),
            _ => {}
        }
    }
    Ok(ParsedRestoreTransactionLog {
        plan_id: plan_id.ok_or_else(|| {
            EvaError::conflict("restore transaction log missing plan_id")
                .with_context("transaction_log", path.display().to_string())
        })?,
        preflight_hash: preflight_hash.ok_or_else(|| {
            EvaError::conflict("restore transaction log missing preflight_hash")
                .with_context("transaction_log", path.display().to_string())
        })?,
        status: status.ok_or_else(|| {
            EvaError::conflict("restore transaction log missing status")
                .with_context("transaction_log", path.display().to_string())
        })?,
        mutation_executed: mutation_executed.ok_or_else(|| {
            EvaError::conflict("restore transaction log missing mutation_executed")
                .with_context("transaction_log", path.display().to_string())
        })?,
        entries,
    })
}

/// 解析以竖线分隔的六字段事务步骤记录。
fn parse_restore_transaction_step(
    value: &str,
    path: &Path,
) -> Result<RestoreMutationTransactionEntry, EvaError> {
    let parts = value.split('|').collect::<Vec<_>>();
    if parts.len() != 6 {
        return Err(
            EvaError::conflict("restore transaction step must have six fields")
                .with_context("transaction_log", path.display().to_string())
                .with_context("step", value),
        );
    }
    let sequence = parts[0].parse::<usize>().map_err(|error| {
        EvaError::conflict("restore transaction step sequence is invalid")
            .with_context("transaction_log", path.display().to_string())
            .with_context("sequence", parts[0])
            .with_context("parse_error", error.to_string())
    })?;
    Ok(RestoreMutationTransactionEntry {
        sequence,
        operation: parts[1].to_owned(),
        relative_path: validate_restore_relative_path(
            "transaction_relative_path",
            parts[2].to_owned(),
        )?,
        status: parts[3].to_owned(),
        digest: match parts[4] {
            "none" => None,
            value => Some(validate_restore_digest(
                "transaction_digest",
                value.to_owned(),
            )?),
        },
        message: match parts[5] {
            "none" => None,
            value => Some(value.to_owned()),
        },
    })
}

/// 一个需要逆向处理的正向步骤及其特殊失败阶段。
#[derive(Debug, Clone, PartialEq, Eq)]
struct RestoreRollbackCandidate {
    /// 原始暂存计划中的正向步骤。
    step: RestoreMutationStep,
    /// Replace 是否可能已删除旧文件但未提交新文件。
    failed_replace: bool,
}

/// 交叉验证事务条目与暂存计划，并按逆序生成回滚候选。
///
/// 正常只回滚 committed 步骤；若 Replace 的失败报告表明已经发生变更，也必须加入
/// 候选，以恢复可能已被删除的旧文件。任何序号、路径或操作不一致均视为日志篡改。
fn rollback_candidates(
    plan: &RestoreStagedMutationPlan,
    transaction: &ParsedRestoreTransactionLog,
) -> Result<Vec<RestoreRollbackCandidate>, EvaError> {
    let mut candidates = Vec::new();
    for entry in transaction.entries.iter().rev() {
        let step = plan.steps.get(entry.sequence).ok_or_else(|| {
            EvaError::conflict("restore transaction references unknown mutation step")
                .with_context("plan_id", &plan.plan_id)
                .with_context("sequence", entry.sequence.to_string())
        })?;
        if step.relative_path != entry.relative_path || step.operation.as_str() != entry.operation {
            return Err(
                EvaError::conflict("restore transaction step does not match staged plan")
                    .with_context("plan_id", &plan.plan_id)
                    .with_context("sequence", entry.sequence.to_string())
                    .with_context("transaction_relative_path", &entry.relative_path)
                    .with_context("plan_relative_path", &step.relative_path),
            );
        }
        if entry.status == "committed" {
            candidates.push(RestoreRollbackCandidate {
                step: step.clone(),
                failed_replace: false,
            });
        } else if entry.status == "failed"
            && transaction.mutation_executed
            && step.operation == RestoreMutationOperation::Replace
        {
            candidates.push(RestoreRollbackCandidate {
                step: step.clone(),
                failed_replace: true,
            });
        }
    }
    if candidates.is_empty() {
        return Err(
            EvaError::conflict("restore rollback has no committed mutation steps")
                .with_context("plan_id", &plan.plan_id),
        );
    }
    Ok(candidates)
}

/// 返回正向步骤对应的稳定回滚操作名。
fn rollback_operation_name(step: &RestoreMutationStep) -> &'static str {
    match step.operation {
        RestoreMutationOperation::Copy => "rollback_delete",
        RestoreMutationOperation::Delete | RestoreMutationOperation::Replace => "rollback_restore",
    }
}

/// 执行一个逆向步骤，并在覆盖或删除前验证当前状态未发生漂移。
///
/// Copy 的回滚删除正向写入的新文件；Delete/Replace 从前置归档恢复旧字节。后两者
/// 同时验证归档条目摘要与计划旧摘要，防止使用错误前置备份。
fn apply_restore_rollback_step(
    sequence: usize,
    root: &Path,
    candidate: &RestoreRollbackCandidate,
    pre_restore_archive: &RestorePreRestoreArchive,
) -> Result<RestoreMutationTransactionEntry, EvaError> {
    let step = &candidate.step;
    let target_path = checked_restore_target_path(root, &step.relative_path)?;
    match step.operation {
        RestoreMutationOperation::Copy => {
            verify_current_digest_for_rollback(
                &target_path,
                step.expected_digest.as_deref(),
                step,
            )?;
            fs::remove_file(&target_path).map_err(|error| {
                EvaError::internal("failed to delete copied restore target during rollback")
                    .with_context("relative_path", &step.relative_path)
                    .with_context("target_path", target_path.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
            Ok(rollback_committed_entry(sequence, step, None))
        }
        RestoreMutationOperation::Delete | RestoreMutationOperation::Replace => {
            let entry = pre_restore_archive
                .entry(&step.relative_path)
                .ok_or_else(|| {
                    EvaError::not_found("pre-restore archive entry is missing for rollback")
                        .with_context("relative_path", &step.relative_path)
                        .with_context("artifact_key", &pre_restore_archive.artifact_key)
                })?;
            let expected_digest = step.pre_restore_digest.as_deref().ok_or_else(|| {
                EvaError::invalid_argument("restore rollback pre-restore digest is required")
                    .with_context("relative_path", &step.relative_path)
            })?;
            if entry.digest != expected_digest {
                return Err(
                    EvaError::conflict("pre-restore archive entry digest mismatch")
                        .with_context("relative_path", &step.relative_path)
                        .with_context("expected_digest", expected_digest)
                        .with_context("actual_digest", &entry.digest),
                );
            }
            verify_restore_rollback_restore_target(
                &target_path,
                step,
                expected_digest,
                candidate.failed_replace,
            )?;
            write_restore_bytes_atomically(
                &target_path,
                &step.relative_path,
                sequence,
                &entry.bytes,
            )?;
            Ok(rollback_committed_entry(
                sequence,
                step,
                Some(entry.digest.clone()),
            ))
        }
    }
}

/// 在回滚删除新复制文件前验证其内容仍是计划写入版本。
fn verify_current_digest_for_rollback(
    target_path: &Path,
    expected_digest: Option<&str>,
    step: &RestoreMutationStep,
) -> Result<(), EvaError> {
    let expected_digest = expected_digest.ok_or_else(|| {
        EvaError::invalid_argument("restore rollback expected digest is required")
            .with_context("relative_path", &step.relative_path)
    })?;
    let bytes = fs::read(target_path).map_err(|error| {
        EvaError::conflict("restore rollback target is missing or unreadable")
            .with_context("relative_path", &step.relative_path)
            .with_context("target_path", target_path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    let actual_digest = digest_bytes(&bytes);
    if actual_digest != expected_digest {
        return Err(
            EvaError::conflict("restore rollback target digest mismatch")
                .with_context("relative_path", &step.relative_path)
                .with_context("expected_digest", expected_digest)
                .with_context("actual_digest", actual_digest),
        );
    }
    Ok(())
}

/// 判断 Delete/Replace 的当前目标状态是否可安全恢复旧内容。
///
/// Delete 允许目标仍不存在或已是旧内容；Replace 允许新内容、旧内容，或在已知
/// “删旧后失败”阶段目标不存在。任何第三方写入产生的其他摘要都会阻塞回滚，避免
/// 静默覆盖故障发生后的有效数据。
fn verify_restore_rollback_restore_target(
    target_path: &Path,
    step: &RestoreMutationStep,
    pre_restore_digest: &str,
    failed_replace: bool,
) -> Result<(), EvaError> {
    let current = fs::read(target_path);
    match (step.operation, current) {
        (RestoreMutationOperation::Delete, Err(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(())
        }
        (RestoreMutationOperation::Delete, Ok(bytes)) => {
            let actual = digest_bytes(&bytes);
            if actual == pre_restore_digest {
                Ok(())
            } else {
                Err(
                    EvaError::conflict("restore rollback delete target was recreated")
                        .with_context("relative_path", &step.relative_path)
                        .with_context("expected_digest", pre_restore_digest)
                        .with_context("actual_digest", actual),
                )
            }
        }
        (RestoreMutationOperation::Replace, Err(error))
            if error.kind() == std::io::ErrorKind::NotFound && failed_replace =>
        {
            Ok(())
        }
        (RestoreMutationOperation::Replace, Ok(bytes)) => {
            let actual = digest_bytes(&bytes);
            let expected_new = step.expected_digest.as_deref().ok_or_else(|| {
                EvaError::invalid_argument("restore rollback expected digest is required")
                    .with_context("relative_path", &step.relative_path)
            })?;
            if actual == expected_new || actual == pre_restore_digest {
                Ok(())
            } else {
                Err(
                    EvaError::conflict("restore rollback replace target digest mismatch")
                        .with_context("relative_path", &step.relative_path)
                        .with_context("expected_digest", expected_new)
                        .with_context("pre_restore_digest", pre_restore_digest)
                        .with_context("actual_digest", actual),
                )
            }
        }
        (_, Err(error)) => Err(EvaError::conflict(
            "restore rollback target is missing or unreadable",
        )
        .with_context("relative_path", &step.relative_path)
        .with_context("target_path", target_path.display().to_string())
        .with_context("io_error", error.to_string())),
        _ => Ok(()),
    }
}

/// 构造一个成功提交的逆向事务日志条目。
fn rollback_committed_entry(
    sequence: usize,
    step: &RestoreMutationStep,
    digest: Option<String>,
) -> RestoreMutationTransactionEntry {
    RestoreMutationTransactionEntry {
        sequence,
        operation: rollback_operation_name(step).to_owned(),
        relative_path: step.relative_path.clone(),
        status: "committed".to_owned(),
        digest,
        message: None,
    }
}

/// 解析前置归档条目的非负字节数。
fn parse_archive_entry_size(value: &str, artifact_key: &str) -> Result<usize, EvaError> {
    value.parse::<usize>().map_err(|error| {
        EvaError::conflict("pre-restore archive entry size is invalid")
            .with_context("artifact_key", artifact_key)
            .with_context("size", value)
            .with_context("parse_error", error.to_string())
    })
}

/// 将偶数字符长度的十六进制文本解码为原始字节。
fn hex_decode(value: &str) -> Result<Vec<u8>, EvaError> {
    if !value.len().is_multiple_of(2) {
        return Err(EvaError::conflict("hex payload length must be even"));
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for chunk in value.as_bytes().chunks_exact(2) {
        let high = hex_nibble(chunk[0])?;
        let low = hex_nibble(chunk[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

/// 将单个 ASCII 十六进制字符转换为四位数值。
fn hex_nibble(byte: u8) -> Result<u8, EvaError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(
            EvaError::conflict("hex payload contains an invalid character")
                .with_context("byte", char::from(byte).to_string()),
        ),
    }
}

/// 将变更前失败包装为尚未发生文件系统变更的步骤错误。
fn step_error(error: EvaError) -> RestoreMutationStepFailure {
    RestoreMutationStepFailure {
        error,
        mutation_executed: false,
    }
}

/// 从规范化根目录逐段构造目标路径，并拒绝任何现存符号链接组件。
///
/// 该检查覆盖中间目录和最终目标，防止语法合法的相对路径经符号链接逃逸根目录。
/// 检查与后续写入之间仍存在文件系统竞态，因此调用方应在受控、无不可信并发写入
/// 的恢复窗口中运行引擎。
fn checked_restore_target_path(root: &Path, relative_path: &str) -> Result<PathBuf, EvaError> {
    let mut cursor = root.to_path_buf();
    for segment in relative_path.split('/') {
        cursor.push(segment);
        if let Ok(metadata) = fs::symlink_metadata(&cursor) {
            if metadata.file_type().is_symlink() {
                return Err(EvaError::permission_denied(
                    "restore mutation target path cannot traverse symlinks",
                )
                .with_context("relative_path", relative_path)
                .with_context("target_path", cursor.display().to_string()));
            }
        }
    }
    Ok(cursor)
}

/// 查找源工件，并同时验证实际字节、记录摘要和步骤预期摘要。
fn checked_restore_source_artifact<'a>(
    step: &RestoreMutationStep,
    source_artifacts: &'a BTreeMap<String, ArtifactRecord>,
) -> Result<&'a [u8], EvaError> {
    let source_key = step.source_artifact_key.as_deref().ok_or_else(|| {
        EvaError::invalid_argument("restore mutation source artifact is required")
            .with_context("relative_path", &step.relative_path)
    })?;
    let artifact = source_artifacts.get(source_key).ok_or_else(|| {
        EvaError::not_found("restore mutation source artifact is missing")
            .with_context("artifact_key", source_key)
            .with_context("relative_path", &step.relative_path)
    })?;
    let expected_digest = step.expected_digest.as_deref().ok_or_else(|| {
        EvaError::invalid_argument("restore mutation expected digest is required")
            .with_context("relative_path", &step.relative_path)
    })?;
    let actual_digest = digest_bytes(&artifact.bytes);
    if actual_digest != artifact.digest || artifact.digest != expected_digest {
        return Err(
            EvaError::conflict("restore mutation source artifact digest mismatch")
                .with_context("artifact_key", source_key)
                .with_context("expected_digest", expected_digest)
                .with_context("actual_digest", actual_digest)
                .with_context("record_digest", &artifact.digest),
        );
    }
    Ok(&artifact.bytes)
}

/// 在删除或替换前验证目标仍与演练时的恢复前摘要一致。
fn verify_existing_target_digest(
    target_path: &Path,
    step: &RestoreMutationStep,
) -> Result<(), EvaError> {
    let expected = step.pre_restore_digest.as_deref().ok_or_else(|| {
        EvaError::invalid_argument("restore mutation pre-restore digest is required")
            .with_context("relative_path", &step.relative_path)
    })?;
    let bytes = fs::read(target_path).map_err(|error| {
        let message = if error.kind() == std::io::ErrorKind::NotFound {
            "restore mutation target is missing"
        } else {
            "failed to read restore mutation target"
        };
        EvaError::conflict(message)
            .with_context("relative_path", &step.relative_path)
            .with_context("target_path", target_path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    let actual = digest_bytes(&bytes);
    if actual != expected {
        return Err(
            EvaError::conflict("restore mutation target pre-restore digest mismatch")
                .with_context("relative_path", &step.relative_path)
                .with_context("expected_digest", expected)
                .with_context("actual_digest", actual),
        );
    }
    Ok(())
}

/// 通过同目录临时文件和 rename 提交新文件内容。
///
/// 同目录 rename 在常见本地文件系统中提供单路径可见性的原子替换，但本函数不执行
/// fsync，因此不承诺断电持久性。Copy 路径会先确保目标不存在；失败时尽力清理临时
/// 文件，事务日志仍是恢复判断依据。
fn write_restore_bytes_atomically(
    target_path: &Path,
    relative_path: &str,
    sequence: usize,
    bytes: &[u8],
) -> Result<(), EvaError> {
    let parent = target_path.parent().ok_or_else(|| {
        EvaError::invalid_argument("restore mutation target must have a parent directory")
            .with_context("relative_path", relative_path)
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        EvaError::internal("failed to create restore mutation target directory")
            .with_context("relative_path", relative_path)
            .with_context("target_path", target_path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    let temp_path = parent.join(format!(".eva-restore-{sequence}.tmp"));
    if temp_path.exists() {
        fs::remove_file(&temp_path).map_err(|error| {
            EvaError::internal("failed to clear stale restore mutation temp file")
                .with_context("temp_path", temp_path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
    }
    fs::write(&temp_path, bytes).map_err(|error| {
        EvaError::internal("failed to write restore mutation temp file")
            .with_context("relative_path", relative_path)
            .with_context("temp_path", temp_path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    fs::rename(&temp_path, target_path).map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        EvaError::internal("failed to commit restore mutation target")
            .with_context("relative_path", relative_path)
            .with_context("target_path", target_path.display().to_string())
            .with_context("io_error", error.to_string())
    })
}

/// 表示替换失败以及旧目标是否已经被删除。
struct ReplaceFailure {
    /// 带文件路径上下文的实际错误。
    error: EvaError,
    /// 删除旧目标后提交新文件失败时为 `true`。
    mutation_executed: bool,
}

/// 暂存新内容、删除旧目标，再以 rename 提交替换。
///
/// Windows 等平台无法直接 rename 覆盖现有文件，因此这里显式先删除旧文件。这意味
/// 替换在删除和 rename 之间不是原子的；若第二步失败会标记 `mutation_executed=true`，
/// 强制上层使用前置备份回滚，不能把失败误判为未发生变更。
fn replace_restore_target_atomically(
    target_path: &Path,
    relative_path: &str,
    sequence: usize,
    bytes: &[u8],
) -> Result<(), ReplaceFailure> {
    let parent = target_path.parent().ok_or_else(|| ReplaceFailure {
        error: EvaError::invalid_argument("restore mutation target must have a parent directory")
            .with_context("relative_path", relative_path),
        mutation_executed: false,
    })?;
    let temp_path = parent.join(format!(".eva-restore-{sequence}.tmp"));
    if temp_path.exists() {
        fs::remove_file(&temp_path).map_err(|error| ReplaceFailure {
            error: EvaError::internal("failed to clear stale restore mutation temp file")
                .with_context("temp_path", temp_path.display().to_string())
                .with_context("io_error", error.to_string()),
            mutation_executed: false,
        })?;
    }
    fs::write(&temp_path, bytes).map_err(|error| ReplaceFailure {
        error: EvaError::internal("failed to write restore mutation temp file")
            .with_context("relative_path", relative_path)
            .with_context("temp_path", temp_path.display().to_string())
            .with_context("io_error", error.to_string()),
        mutation_executed: false,
    })?;
    fs::remove_file(target_path).map_err(|error| ReplaceFailure {
        error: EvaError::internal("failed to remove existing restore target")
            .with_context("relative_path", relative_path)
            .with_context("target_path", target_path.display().to_string())
            .with_context("io_error", error.to_string()),
        mutation_executed: false,
    })?;
    fs::rename(&temp_path, target_path).map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        ReplaceFailure {
            error: EvaError::internal("failed to commit replacement restore target")
                .with_context("relative_path", relative_path)
                .with_context("target_path", target_path.display().to_string())
                .with_context("io_error", error.to_string()),
            mutation_executed: true,
        }
    })
}

/// 构造一个成功提交的正向事务日志条目。
fn committed_entry(
    sequence: usize,
    step: &RestoreMutationStep,
    digest: Option<String>,
) -> RestoreMutationTransactionEntry {
    RestoreMutationTransactionEntry {
        sequence,
        operation: step.operation.as_str().to_owned(),
        relative_path: step.relative_path.clone(),
        status: "committed".to_owned(),
        digest,
        message: None,
    }
}

/// 以追加方式写入转义后的单个事务步骤记录。
///
/// 日志写入失败会中断流程；当前实现不调用 fsync，进程崩溃时最后一条记录可能未
/// 持久化，恢复工具应结合文件系统状态和 started 记录保守判断。
fn append_restore_transaction_log(
    path: &Path,
    entry: &RestoreMutationTransactionEntry,
) -> Result<(), EvaError> {
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .map_err(|error| {
            EvaError::internal("failed to open restore mutation transaction log")
                .with_context("transaction_log", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
    writeln!(
        file,
        "step={}|{}|{}|{}|{}|{}",
        entry.sequence,
        stable_log_value(&entry.operation),
        stable_log_value(&entry.relative_path),
        stable_log_value(&entry.status),
        entry.digest.as_deref().unwrap_or("none"),
        entry
            .message
            .as_deref()
            .map(stable_log_value)
            .unwrap_or_else(|| "none".to_owned())
    )
    .map_err(|error| {
        EvaError::internal("failed to write restore mutation transaction log")
            .with_context("transaction_log", path.display().to_string())
            .with_context("io_error", error.to_string())
    })
}

/// 向事务日志追加总体状态和是否已变更标记。
fn append_restore_transaction_status(
    path: &Path,
    status: &str,
    mutation_executed: bool,
) -> Result<(), EvaError> {
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .map_err(|error| {
            EvaError::internal("failed to open restore mutation transaction log")
                .with_context("transaction_log", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
    writeln!(
        file,
        "status={}\nmutation_executed={}",
        stable_log_value(status),
        mutation_executed
    )
    .map_err(|error| {
        EvaError::internal("failed to write restore mutation transaction status")
            .with_context("transaction_log", path.display().to_string())
            .with_context("io_error", error.to_string())
    })
}

/// 转义会破坏行式日志字段边界的换行和竖线字符。
fn stable_log_value(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '\n' | '\r' | '|' => '_',
            _ => character,
        })
        .collect()
}

/// 校验所有者并构造与计划和备份绑定的恢复锁证据。
fn build_restore_lock(plan: &RestoreApplyPlan, owner: &str) -> Result<RestoreApplyLock, EvaError> {
    let owner = validate_token("owner", owner.to_owned())?;
    Ok(RestoreApplyLock {
        lock_id: plan.lock_id(),
        plan_id: plan.plan_id.clone(),
        owner,
        status: "acquired".to_owned(),
        audit: vec![
            "lock:acquired".to_owned(),
            format!("plan:{}", plan.plan_id),
            format!("backup:{}", plan.backup_artifact_id),
        ],
    })
}

/// 返回指定计划的恢复应用锁文件路径。
fn restore_lock_path(root: &Path, plan_id: &str) -> PathBuf {
    root.join(format!("{plan_id}.restore.lock"))
}

/// 返回指定计划的独立恢复回滚锁文件路径。
fn restore_rollback_lock_path(root: &Path, plan_id: &str) -> PathBuf {
    root.join(format!("{plan_id}.restore.rollback.lock"))
}

/// 将锁、计划、来源备份和前置备份编码为行式审计载荷。
fn restore_lock_payload(plan: &RestoreApplyPlan, lock: &RestoreApplyLock) -> String {
    let pre_restore = plan
        .pre_restore_backup
        .as_ref()
        .map(|evidence| evidence.backup_artifact_id.as_str())
        .unwrap_or("<missing>");
    format!(
        "lock_id={}\nplan_id={}\nowner={}\nbackup_artifact_id={}\npre_restore_backup_artifact_id={}\nstatus={}\n",
        lock.lock_id,
        plan.plan_id,
        lock.owner,
        plan.backup_artifact_id,
        pre_restore,
        lock.status
    )
}

/// 构造恢复应用锁已存在的冲突错误。
fn restore_lock_conflict(plan_id: &str, lock_path: Option<&Path>) -> EvaError {
    let mut error =
        EvaError::conflict("restore apply lock already exists").with_context("plan_id", plan_id);
    if let Some(lock_path) = lock_path {
        error = error.with_context("lock_path", lock_path.display().to_string());
    }
    error
}

/// 构造恢复回滚锁已存在的冲突错误。
fn restore_rollback_lock_conflict(plan_id: &str, lock_path: &Path) -> EvaError {
    EvaError::conflict("restore rollback lock already exists")
        .with_context("plan_id", plan_id)
        .with_context("lock_path", lock_path.display().to_string())
}

/// 根据健康门禁结果生成恢复应用阶段的可审计步骤列表。
fn restore_apply_steps(health_passed: bool) -> Vec<String> {
    let mut steps = vec![
        "match restore apply confirmation to plan id".to_owned(),
        "verify signed backup artifact and pre-restore evidence".to_owned(),
        "require runtime policy approval for restore.apply".to_owned(),
        "acquire restore apply lock".to_owned(),
        "run pre-apply health check".to_owned(),
    ];
    if health_passed {
        steps.push("gate destructive restore execution".to_owned());
        steps.push("emit staged restore apply audit".to_owned());
    } else {
        steps.push("block destructive restore execution".to_owned());
        steps.push("emit rollback-required audit".to_owned());
    }
    steps
}

#[cfg(test)]
/// 恢复演练、文件事务、回滚以及策略和锁门禁的端到端测试。
mod tests {
    use super::*;
    use eva_policy::{HighRiskAction, PolicyDecision};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 创建进程与时间戳隔离的临时文件系统根目录。
    fn test_temp_dir(name: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("eva-backup-{name}-{}-{now}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        path
    }

    /// 构造允许恢复应用高风险动作的测试策略决策。
    fn allowed_policy() -> PolicyDecision {
        PolicyDecision {
            action: HighRiskAction::RestoreApply,
            allowed: true,
            reason: "explicitly allowed by test".to_owned(),
            audit: vec!["runtime:restore.apply:allowed".to_owned()],
        }
    }

    /// 构造拒绝恢复应用高风险动作的测试策略决策。
    fn denied_policy() -> PolicyDecision {
        PolicyDecision {
            action: HighRiskAction::RestoreApply,
            allowed: false,
            reason: "restore.apply requires explicit approval".to_owned(),
            audit: vec!["runtime:restore.apply:denied".to_owned()],
        }
    }

    /// 构造摘要匹配的恢复计划、来源工件和前置备份工件。
    fn matching_plan_and_artifacts() -> (RestoreApplyPlan, ArtifactRecord, ArtifactRecord) {
        let artifact = ArtifactRecord::new("backup/backup-1", b"ok".as_slice());
        let pre_restore = ArtifactRecord::new("backup/pre-restore-1", b"before".as_slice());
        let plan = RestoreApplyPlan::new("plan-1", "backup-1", artifact.digest.clone())
            .unwrap()
            .with_pre_restore_backup(
                PreRestoreBackupEvidence::new("pre-restore-1", pre_restore.digest.clone()).unwrap(),
            );
        (plan, artifact, pre_restore)
    }

    #[test]
    /// 验证恢复来源摘要不匹配时演练失败关闭。
    fn dry_run_rejects_digest_mismatch() {
        let plan = RestoreApplyPlan::new("plan-1", "backup-1", "sha256:wrong").unwrap();
        let artifact = ArtifactRecord::new("backup/backup-1", b"ok".as_slice());
        let pre_restore = ArtifactRecord::new("backup/pre-restore-1", b"before".as_slice());
        let plan = plan.with_pre_restore_backup(
            PreRestoreBackupEvidence::new("pre-restore-1", pre_restore.digest.clone()).unwrap(),
        );

        let error = RestoreApplyValidator
            .dry_run(&plan, &artifact, Some(&pre_restore))
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    /// 验证演练同时校验恢复来源和前置安全备份。
    fn dry_run_validates_matching_backup_and_pre_restore_evidence() {
        let digest = "sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df";
        let pre_restore = ArtifactRecord::new("backup/pre-restore-1", b"before".as_slice());
        let plan = RestoreApplyPlan::new("plan-1", "backup-1", digest)
            .unwrap()
            .with_pre_restore_backup(
                PreRestoreBackupEvidence::new("pre-restore-1", pre_restore.digest.clone()).unwrap(),
            );
        let artifact = ArtifactRecord::new("backup/backup-1", b"ok".as_slice());

        let report = RestoreApplyValidator
            .dry_run(&plan, &artifact, Some(&pre_restore))
            .unwrap();

        assert_eq!(report.status, "dry_run_validated");
        assert!(!report.apply_allowed);
        assert_eq!(
            report.pre_restore_backup_artifact_key,
            "backup/pre-restore-1"
        );
        assert!(!report.mutation_plan.mutation_executed);
        assert_eq!(report.mutation_plan.audit[0], "restore.mutation:plan_only");
    }

    #[test]
    /// 验证暂存规划的预览、预检摘要和回滚清单可复现。
    fn staged_mutation_planner_builds_reproducible_preview_and_rollback_manifest() {
        let artifact = ArtifactRecord::new("backup/backup-1", b"archive".as_slice());
        let pre_restore = ArtifactRecord::new("backup/pre-restore-1", b"before".as_slice());
        let copy_digest = ArtifactRecord::new("backup/config", b"config".as_slice()).digest;
        let replace_digest = ArtifactRecord::new("backup/bin", b"binary".as_slice()).digest;
        let delete_digest = ArtifactRecord::new("backup/log", b"log".as_slice()).digest;
        let replaced_digest =
            ArtifactRecord::new("backup/old-bin", b"old-binary".as_slice()).digest;
        let plan = RestoreApplyPlan::new("plan-1", "backup-1", artifact.digest)
            .unwrap()
            .with_pre_restore_backup(
                PreRestoreBackupEvidence::new("pre-restore-1", pre_restore.digest).unwrap(),
            )
            .with_mutation_target_root("workspace")
            .unwrap()
            .with_mutation_steps(vec![
                RestoreMutationStep::copy_file(
                    "config/eva.yaml",
                    "backup/config",
                    copy_digest.clone(),
                )
                .unwrap(),
                RestoreMutationStep::delete_file("logs/old.log", delete_digest.clone()).unwrap(),
                RestoreMutationStep::replace_file(
                    "bin/eva",
                    "backup/bin",
                    replace_digest.clone(),
                    replaced_digest.clone(),
                )
                .unwrap(),
            ]);

        let staged = RestoreStagedMutationPlanner.plan(&plan).unwrap();
        let staged_again = RestoreStagedMutationPlanner.plan(&plan).unwrap();

        assert!(staged.mutation_planned);
        assert!(!staged.mutation_executed);
        assert_eq!(staged.preflight_hash, staged_again.preflight_hash);
        assert_eq!(
            staged.affected_paths,
            vec![
                "bin/eva".to_owned(),
                "config/eva.yaml".to_owned(),
                "logs/old.log".to_owned()
            ]
        );
        assert_eq!(staged.preview.len(), 3);
        assert!(staged.preview[0].contains("copy config/eva.yaml"));
        assert_eq!(staged.rollback_manifest[0].action, "delete_restored_path");
        assert_eq!(
            staged.rollback_manifest[1].pre_restore_digest.as_deref(),
            Some(delete_digest.as_str())
        );
        assert_eq!(
            staged.rollback_manifest[2].pre_restore_digest.as_deref(),
            Some(replaced_digest.as_str())
        );
    }

    #[test]
    /// 验证路径逃逸和符号链接目标在执行前被拒绝。
    fn staged_mutation_planner_rejects_path_escape_and_symlink_targets() {
        let digest = ArtifactRecord::new("backup/config", b"config".as_slice()).digest;

        let traversal =
            RestoreMutationStep::copy_file("../secret", "backup/config", digest.clone())
                .unwrap_err();
        assert_eq!(traversal.kind(), eva_core::ErrorKind::InvalidArgument);

        let absolute =
            RestoreMutationStep::copy_file("/tmp/secret", "backup/config", digest).unwrap_err();
        assert_eq!(absolute.kind(), eva_core::ErrorKind::InvalidArgument);

        let windows_prefix = RestoreMutationStep::delete_file(
            "C:/eva/secret",
            ArtifactRecord::new("backup/secret", b"secret".as_slice()).digest,
        )
        .unwrap_err();
        assert_eq!(windows_prefix.kind(), eva_core::ErrorKind::InvalidArgument);

        let symlink = RestoreMutationTargetKind::parse("symlink").unwrap_err();
        assert_eq!(symlink.kind(), eva_core::ErrorKind::InvalidArgument);
    }

    #[test]
    /// 验证复制、删除、替换按序执行并留下事务日志。
    fn mutation_engine_applies_staged_copy_delete_replace_with_transaction_log() {
        let target_root = test_temp_dir("mutation-apply");
        fs::create_dir_all(target_root.join("bin")).unwrap();
        fs::create_dir_all(target_root.join("logs")).unwrap();
        fs::write(target_root.join("bin/eva"), b"old-binary").unwrap();
        fs::write(target_root.join("logs/old.log"), b"old-log").unwrap();
        let backup = ArtifactRecord::new("backup/backup-1", b"archive".as_slice());
        let pre_restore = ArtifactRecord::new("backup/pre-restore-1", b"before".as_slice());
        let config = ArtifactRecord::new("backup/config", b"config".as_slice());
        let binary = ArtifactRecord::new("backup/bin", b"binary".as_slice());
        let old_binary_digest = digest_bytes(b"old-binary");
        let old_log_digest = digest_bytes(b"old-log");
        let plan = RestoreApplyPlan::new("plan-apply", "backup-1", backup.digest)
            .unwrap()
            .with_pre_restore_backup(
                PreRestoreBackupEvidence::new("pre-restore-1", pre_restore.digest).unwrap(),
            )
            .with_mutation_target_root(target_root.display().to_string())
            .unwrap()
            .with_mutation_steps(vec![
                RestoreMutationStep::copy_file(
                    "config/eva.yaml",
                    "backup/config",
                    config.digest.clone(),
                )
                .unwrap(),
                RestoreMutationStep::replace_file(
                    "bin/eva",
                    "backup/bin",
                    binary.digest.clone(),
                    old_binary_digest,
                )
                .unwrap(),
                RestoreMutationStep::delete_file("logs/old.log", old_log_digest).unwrap(),
            ]);
        let staged = RestoreStagedMutationPlanner.plan(&plan).unwrap();
        let transaction_log_path = target_root.join(".eva/plan-apply.restore.txn");
        let mut sources = BTreeMap::new();
        sources.insert(config.key.clone(), config);
        sources.insert(binary.key.clone(), binary);

        let report = RestoreMutationEngine
            .apply(&staged, &target_root, &transaction_log_path, &sources)
            .unwrap();

        assert_eq!(report.status, "applied");
        assert!(report.mutation_executed);
        assert!(!report.rollback_required);
        assert_eq!(report.completed_steps, 3);
        assert_eq!(
            fs::read(target_root.join("config/eva.yaml")).unwrap(),
            b"config"
        );
        assert_eq!(fs::read(target_root.join("bin/eva")).unwrap(), b"binary");
        assert!(!target_root.join("logs/old.log").exists());
        let transaction_log = fs::read_to_string(transaction_log_path).unwrap();
        assert!(transaction_log.contains("status=applied"));
        assert!(transaction_log.contains("step=0|copy|config/eva.yaml|committed"));

        fs::remove_dir_all(target_root).unwrap();
    }

    #[test]
    /// 验证首个失败停止后续步骤并显式要求回滚。
    fn mutation_engine_stops_on_failure_and_marks_rollback_required() {
        let target_root = test_temp_dir("mutation-failure");
        fs::create_dir_all(target_root.join("bin")).unwrap();
        fs::create_dir_all(target_root.join("logs")).unwrap();
        fs::write(target_root.join("bin/eva"), b"old-binary").unwrap();
        fs::write(target_root.join("logs/old.log"), b"unexpected-log").unwrap();
        let backup = ArtifactRecord::new("backup/backup-1", b"archive".as_slice());
        let pre_restore = ArtifactRecord::new("backup/pre-restore-1", b"before".as_slice());
        let binary = ArtifactRecord::new("backup/bin", b"binary".as_slice());
        let old_binary_digest = digest_bytes(b"old-binary");
        let old_log_digest = digest_bytes(b"old-log");
        let plan = RestoreApplyPlan::new("plan-failure", "backup-1", backup.digest)
            .unwrap()
            .with_pre_restore_backup(
                PreRestoreBackupEvidence::new("pre-restore-1", pre_restore.digest).unwrap(),
            )
            .with_mutation_target_root(target_root.display().to_string())
            .unwrap()
            .with_mutation_steps(vec![
                RestoreMutationStep::replace_file(
                    "bin/eva",
                    "backup/bin",
                    binary.digest.clone(),
                    old_binary_digest,
                )
                .unwrap(),
                RestoreMutationStep::delete_file("logs/old.log", old_log_digest).unwrap(),
            ]);
        let staged = RestoreStagedMutationPlanner.plan(&plan).unwrap();
        let transaction_log_path = target_root.join(".eva/plan-failure.restore.txn");
        let mut sources = BTreeMap::new();
        sources.insert(binary.key.clone(), binary);

        let report = RestoreMutationEngine
            .apply(&staged, &target_root, &transaction_log_path, &sources)
            .unwrap();

        assert_eq!(report.status, "rollback_required");
        assert!(report.mutation_executed);
        assert!(report.rollback_required);
        assert_eq!(report.completed_steps, 1);
        assert_eq!(report.failed_step.as_deref(), Some("logs/old.log"));
        assert_eq!(fs::read(target_root.join("bin/eva")).unwrap(), b"binary");
        assert_eq!(
            fs::read(target_root.join("logs/old.log")).unwrap(),
            b"unexpected-log"
        );
        let transaction_log = fs::read_to_string(transaction_log_path).unwrap();
        assert!(transaction_log.contains("status=rollback_required"));
        assert!(transaction_log.contains("logs/old.log|failed"));

        fs::remove_dir_all(target_root).unwrap();
    }

    #[test]
    /// 验证回滚引擎按逆序从前置归档恢复已提交步骤。
    fn rollback_engine_restores_committed_steps_from_pre_restore_archive() {
        let target_root = test_temp_dir("rollback-apply");
        fs::create_dir_all(target_root.join("bin")).unwrap();
        fs::create_dir_all(target_root.join("logs")).unwrap();
        let backup = ArtifactRecord::new("backup/backup-1", b"archive".as_slice());
        let pre_restore = ArtifactRecord::new(
            "backup/pre-restore-1",
            b"eva-backup-archive:v1\nentry.path=bin/eva\nentry.size=10\nentry.redacted=false\nentry.bytes.hex=6f6c642d62696e617279\nentry.path=logs/old.log\nentry.size=7\nentry.redacted=false\nentry.bytes.hex=6f6c642d6c6f67\n"
                .as_slice(),
        );
        let binary = ArtifactRecord::new("backup/bin", b"binary".as_slice());
        let old_binary_digest = digest_bytes(b"old-binary");
        let old_log_digest = digest_bytes(b"old-log");
        let plan = RestoreApplyPlan::new("plan-rollback", "backup-1", backup.digest)
            .unwrap()
            .with_pre_restore_backup(
                PreRestoreBackupEvidence::new("pre-restore-1", pre_restore.digest.clone()).unwrap(),
            )
            .with_mutation_target_root(target_root.display().to_string())
            .unwrap()
            .with_mutation_steps(vec![
                RestoreMutationStep::replace_file(
                    "bin/eva",
                    "backup/bin",
                    binary.digest.clone(),
                    old_binary_digest,
                )
                .unwrap(),
                RestoreMutationStep::delete_file("logs/old.log", old_log_digest).unwrap(),
            ]);
        let staged = RestoreStagedMutationPlanner.plan(&plan).unwrap();
        fs::write(target_root.join("bin/eva"), b"binary").unwrap();
        let transaction_log_path = target_root.join(".eva/plan-rollback.restore.txn");
        fs::create_dir_all(transaction_log_path.parent().unwrap()).unwrap();
        fs::write(
            &transaction_log_path,
            format!(
                "restore-mutation-transaction:v1\nplan_id=plan-rollback\ntarget_root={}\npreflight_hash={}\nstep=0|replace|bin/eva|committed|{}|none\nstep=1|delete|logs/old.log|committed|{}|none\nstep=1|delete|logs/old.log|failed|none|digest mismatch\nstatus=rollback_required\nmutation_executed=true\n",
                target_root.display(),
                staged.preflight_hash,
                binary.digest,
                digest_bytes(b"old-log")
            ),
        )
        .unwrap();
        let pre_restore_archive =
            RestorePreRestoreArchive::parse(&pre_restore, &pre_restore.digest).unwrap();

        let report = RestoreRollbackEngine
            .apply(
                &staged,
                &target_root,
                &transaction_log_path,
                target_root.join(".eva/plan-rollback.restore.rollback.txn"),
                &pre_restore_archive,
            )
            .unwrap();

        assert_eq!(report.status, "rolled_back");
        assert!(report.rollback_executed);
        assert_eq!(report.completed_steps, 2);
        assert_eq!(
            fs::read(target_root.join("bin/eva")).unwrap(),
            b"old-binary"
        );
        assert_eq!(
            fs::read(target_root.join("logs/old.log")).unwrap(),
            b"old-log"
        );
        let rollback_log = fs::read_to_string(report.rollback_log_path).unwrap();
        assert!(rollback_log.contains("status=rolled_back"));

        fs::remove_dir_all(target_root).unwrap();
    }

    #[test]
    /// 验证故障后目标内容漂移会阻止回滚覆盖。
    fn rollback_engine_rejects_current_digest_drift() {
        let target_root = test_temp_dir("rollback-drift");
        fs::create_dir_all(target_root.join("bin")).unwrap();
        fs::write(target_root.join("bin/eva"), b"operator-edit").unwrap();
        let backup = ArtifactRecord::new("backup/backup-1", b"archive".as_slice());
        let pre_restore = ArtifactRecord::new(
            "backup/pre-restore-1",
            b"eva-backup-archive:v1\nentry.path=bin/eva\nentry.size=10\nentry.redacted=false\nentry.bytes.hex=6f6c642d62696e617279\n"
                .as_slice(),
        );
        let binary = ArtifactRecord::new("backup/bin", b"binary".as_slice());
        let old_binary_digest = digest_bytes(b"old-binary");
        let plan = RestoreApplyPlan::new("plan-drift", "backup-1", backup.digest)
            .unwrap()
            .with_pre_restore_backup(
                PreRestoreBackupEvidence::new("pre-restore-1", pre_restore.digest.clone()).unwrap(),
            )
            .with_mutation_target_root(target_root.display().to_string())
            .unwrap()
            .with_mutation_steps(vec![RestoreMutationStep::replace_file(
                "bin/eva",
                "backup/bin",
                binary.digest.clone(),
                old_binary_digest,
            )
            .unwrap()]);
        let staged = RestoreStagedMutationPlanner.plan(&plan).unwrap();
        let transaction_log_path = target_root.join(".eva/plan-drift.restore.txn");
        fs::create_dir_all(transaction_log_path.parent().unwrap()).unwrap();
        fs::write(
            &transaction_log_path,
            format!(
                "restore-mutation-transaction:v1\nplan_id=plan-drift\ntarget_root={}\npreflight_hash={}\nstep=0|replace|bin/eva|committed|{}|none\nstatus=rollback_required\nmutation_executed=true\n",
                target_root.display(),
                staged.preflight_hash,
                binary.digest
            ),
        )
        .unwrap();
        let pre_restore_archive =
            RestorePreRestoreArchive::parse(&pre_restore, &pre_restore.digest).unwrap();

        let report = RestoreRollbackEngine
            .apply(
                &staged,
                &target_root,
                &transaction_log_path,
                target_root.join(".eva/plan-drift.restore.rollback.txn"),
                &pre_restore_archive,
            )
            .unwrap();

        assert_eq!(report.status, "rollback_failed");
        assert!(!report.rollback_executed);
        assert_eq!(report.failed_step.as_deref(), Some("bin/eva"));
        assert_eq!(
            fs::read(target_root.join("bin/eva")).unwrap(),
            b"operator-edit"
        );

        fs::remove_dir_all(target_root).unwrap();
    }

    #[test]
    /// 验证缺少恢复前备份证据时演练不能通过。
    fn dry_run_requires_pre_restore_evidence() {
        let digest = "sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df";
        let plan = RestoreApplyPlan::new("plan-1", "backup-1", digest).unwrap();
        let artifact = ArtifactRecord::new("backup/backup-1", b"ok".as_slice());

        let error = RestoreApplyValidator
            .dry_run(&plan, &artifact, None)
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::InvalidArgument);
    }

    #[test]
    /// 验证策略拒绝发生在应用锁获取之前。
    fn restore_apply_gate_requires_policy_approval() {
        let (plan, artifact, pre_restore) = matching_plan_and_artifacts();
        let dry_run = RestoreApplyValidator
            .dry_run(&plan, &artifact, Some(&pre_restore))
            .unwrap();
        let mut store = InMemoryRestoreApplyLockStore::new();

        let error = RestoreApplyCoordinator
            .apply(
                &mut store,
                &plan,
                &dry_run,
                &denied_policy(),
                RestoreApplyHealthCheck::healthy(),
                "cli",
            )
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::PermissionDenied);
    }

    #[test]
    /// 验证双证据、策略和健康通过后只放开执行门禁。
    fn restore_apply_gate_acquires_lock_after_evidence_policy_and_health() {
        let (plan, artifact, pre_restore) = matching_plan_and_artifacts();
        let dry_run = RestoreApplyValidator
            .dry_run(&plan, &artifact, Some(&pre_restore))
            .unwrap();
        let mut store = InMemoryRestoreApplyLockStore::new();

        let report = RestoreApplyCoordinator
            .apply(
                &mut store,
                &plan,
                &dry_run,
                &allowed_policy(),
                RestoreApplyHealthCheck::healthy(),
                "cli",
            )
            .unwrap();

        assert_eq!(report.status, "gated");
        assert!(report.apply_allowed);
        assert!(!report.mutation_executed);
        assert_eq!(report.lock.lock_id, "restore-apply-plan-1");
    }

    #[test]
    /// 验证同一恢复计划的重复应用锁产生冲突。
    fn restore_apply_gate_reports_lock_conflict() {
        let (plan, artifact, pre_restore) = matching_plan_and_artifacts();
        let dry_run = RestoreApplyValidator
            .dry_run(&plan, &artifact, Some(&pre_restore))
            .unwrap();
        let mut store = InMemoryRestoreApplyLockStore::new();

        RestoreApplyCoordinator
            .apply(
                &mut store,
                &plan,
                &dry_run,
                &allowed_policy(),
                RestoreApplyHealthCheck::healthy(),
                "cli",
            )
            .unwrap();
        let error = RestoreApplyCoordinator
            .apply(
                &mut store,
                &plan,
                &dry_run,
                &allowed_policy(),
                RestoreApplyHealthCheck::healthy(),
                "cli",
            )
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    /// 验证获取锁后健康失败仍阻止变更并要求回滚规划。
    fn restore_apply_health_failure_blocks_apply_after_lock() {
        let (plan, artifact, pre_restore) = matching_plan_and_artifacts();
        let dry_run = RestoreApplyValidator
            .dry_run(&plan, &artifact, Some(&pre_restore))
            .unwrap();
        let mut store = InMemoryRestoreApplyLockStore::new();

        let report = RestoreApplyCoordinator
            .apply(
                &mut store,
                &plan,
                &dry_run,
                &allowed_policy(),
                RestoreApplyHealthCheck::failed("pre-restore health failed").unwrap(),
                "cli",
            )
            .unwrap();

        assert_eq!(report.status, "blocked");
        assert!(!report.apply_allowed);
        assert!(report
            .audit
            .iter()
            .any(|entry| entry == "restore.apply:rollback_required"));
    }
}
