//! 恢复计划、应用与回滚子命令；通过确认、备份证据、策略、锁和健康检查保护文件变更。

use super::{
    artifact_store_ref, artifact_store_ref_json, backup_cmd, json_array, json_string,
    lock_store_ref, lock_store_ref_json, parse_common_options, required_option, rollback_plan_json,
    snapshot_cmd, success_envelope, trace_for, write_artifact_store_ref, write_command_error,
    write_error_kind, write_lock_store_ref, write_risk_lines_text, ArtifactStoreRef, CommonOptions,
    LockStoreRef, OutputFormat, EXIT_OK, EXIT_RUNTIME_UNAVAILABLE,
};
use backup_cmd::BackupCreateResult;
use eva_backup::{
    FileSystemRestoreApplyLockStore, PreRestoreBackupEvidence, ReleaseSnapshot,
    ReleaseSnapshotService, RestoreApplyCoordinator, RestoreApplyDryRunReport,
    RestoreApplyHealthCheck, RestoreApplyPlan, RestoreApplyReport, RestoreApplyValidator,
    RestoreMutationApplyReport, RestoreMutationEngine, RestoreMutationOperation,
    RestoreMutationStep, RestoreMutationTargetKind, RestoreMutationTransactionEntry, RestorePlan,
    RestorePreRestoreArchive, RestoreRollbackApplyReport, RestoreRollbackEngine,
    RestoreRollbackEntry, RestoreStagedMutationPlan, SnapshotRole,
};
use eva_config::{load_project_config, ProjectConfig};
use eva_core::{EvaError, GenerationId, RequestId};
use eva_lifecycle::{RollbackCoordinator, RollbackPlan};
use eva_observability::{
    AuditAction, AuditEvent, AuditOutcome, AuditSink, BestEffortObservabilityPipeline, MetricKind,
    MetricLabels, MetricName, MetricPoint, MetricSink, SpanId, TraceFields,
};
use eva_policy::{HighRiskAction, RuntimePolicyGate, RuntimePolicyRequest};
use eva_storage::FileSystemArtifactStore;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Restore 子命令及其已解析选项。
pub(super) enum RestoreCommand {
    /// 从发布前快照生成恢复计划。
    Plan(
        /// 已解析的快照、请求、发布引用、产物存储与公共选项。
        RestorePlanOptions,
    ),
    /// 校验并可选执行 staged restore mutation。
    Apply(
        /// 已解析的计划、确认、存储、锁、健康检查与执行模式。
        RestoreApplyOptions,
    ),
    /// 使用前置备份和事务日志回滚失败的 staged mutation。
    Rollback(
        /// 已解析的计划、确认、存储、锁、事务日志与健康检查选项。
        RestoreRollbackOptions,
    ),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 恢复计划创建选项。
pub(super) struct RestorePlanOptions {
    /// 项目根和输出格式。
    common: CommonOptions,
    /// 作为恢复源的快照 ID。
    snapshot_id: String,
    /// 关联备份、快照和审计的请求 ID。
    request_id: String,
    /// 快照对应的发布引用。
    release_ref: String,
    /// 可选文件系统 ArtifactStore。
    artifact_store: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 恢复应用选项，包含所有高风险门禁输入。
pub(super) struct RestoreApplyOptions {
    /// 项目根和输出格式。
    common: CommonOptions,
    /// 恢复应用计划文件路径。
    plan: Option<PathBuf>,
    /// 必须与 plan ID 匹配的确认令牌。
    confirm: Option<String>,
    /// 备份和 mutation source 所在 ArtifactStore。
    artifact_store: Option<PathBuf>,
    /// 获取 restore apply 锁并保存事务日志的目录。
    lock_store: Option<PathBuf>,
    /// 仅验证证据和计划，不获取 apply 锁或执行变更。
    dry_run: bool,
    /// 写入锁记录的操作者/进程标识。
    owner: String,
    /// 模拟或报告 apply 后健康检查结果。
    health: RestoreApplyHealthOption,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 恢复回滚选项。
pub(super) struct RestoreRollbackOptions {
    /// 项目根和输出格式。
    common: CommonOptions,
    /// 原始 restore apply 计划路径。
    plan: Option<PathBuf>,
    /// 必须与 plan ID 匹配的确认令牌。
    confirm: Option<String>,
    /// 包含目标备份和前置备份的 ArtifactStore。
    artifact_store: Option<PathBuf>,
    /// 获取 rollback 锁和写回滚日志的目录。
    lock_store: Option<PathBuf>,
    /// 可选显式 apply transaction log；缺省从 lock store 推导。
    transaction_log: Option<PathBuf>,
    /// 回滚锁 owner。
    owner: String,
    /// 执行回滚前必须通过的健康门禁。
    health: RestoreApplyHealthOption,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 恢复计划、前置备份与快照的组合结果。
struct RestorePlanResult {
    /// 创建快照前的已验证备份。
    backup: BackupCreateResult,
    /// 发布前快照。
    snapshot: ReleaseSnapshot,
    /// 由快照派生的恢复计划。
    plan: RestorePlan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Restore apply dry-run 的计划、验证报告和存储描述。
struct RestoreApplyDryRunResult {
    /// 从文件解析的强类型 apply 计划。
    plan: RestoreApplyPlan,
    /// 备份、digest、前置证据和 mutation 的验证报告。
    report: RestoreApplyDryRunReport,
    /// 实际读取的 ArtifactStore 描述。
    artifact_store: ArtifactStoreRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Restore apply 的协调器、文件 mutation 和回滚规划结果。
struct RestoreApplyResult {
    /// 锁、策略、健康和 apply 决策报告。
    report: RestoreApplyReport,
    /// apply 前完成的 dry-run 验证证据。
    dry_run: RestoreApplyDryRunReport,
    /// 读取备份的 ArtifactStore。
    artifact_store: ArtifactStoreRef,
    /// 获取 apply 锁的 LockStore。
    lock_store: LockStoreRef,
    /// 计划包含文件步骤时的可选 mutation 事务报告。
    mutation_apply: Option<RestoreMutationApplyReport>,
    /// 健康检查失败时生成的可选代际回滚计划。
    rollback_plan: Option<RollbackPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Restore rollback 的完整证据包。
struct RestoreRollbackResult {
    /// 原始 apply 计划。
    plan: RestoreApplyPlan,
    /// 回滚前复用的 dry-run 验证证据。
    dry_run: RestoreApplyDryRunReport,
    /// 文件回滚引擎报告。
    rollback: RestoreRollbackApplyReport,
    /// 已获取的独占 rollback 锁。
    lock: eva_backup::RestoreApplyLock,
    /// 回滚前健康检查结果。
    health: RestoreApplyHealthCheck,
    /// 前置备份所在 ArtifactStore。
    artifact_store: ArtifactStoreRef,
    /// rollback 锁和日志所在 LockStore。
    lock_store: LockStoreRef,
    /// 聚合策略、证据、锁和引擎的审计条目。
    audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 面向操作者的统一确认摘要，明确计划、变更与回滚事实。
struct RestoreOperatorConfirmation {
    /// restore.apply 或 restore.rollback。
    command: String,
    /// 被确认的 plan ID。
    plan_id: String,
    /// 操作者应核对的确认令牌。
    confirm_token: String,
    /// 文件 mutation 的目标根目录。
    target_root: String,
    /// 受影响文件/步骤数量。
    affected_count: usize,
    /// 所有门禁是否允许 apply。
    apply_allowed: bool,
    /// 计划是否包含 mutation。
    mutation_planned: bool,
    /// mutation 是否已实际执行。
    mutation_executed: bool,
    /// 当前结果是否要求回滚。
    rollback_required: bool,
    /// 回滚是否已实际执行。
    rollback_executed: bool,
    /// 不可逆操作警告；当前实现强调 staged/rollback 边界。
    irreversible_warning: String,
    /// 面向操作者的下一步动作。
    next_action: String,
}

/// 构造统一操作者确认摘要所需的借用输入，避免不同命令字段漂移。
struct RestoreOperatorConfirmationInput<'a> {
    /// 命令名。
    command: &'a str,
    /// 恢复计划标识。
    plan_id: &'a str,
    /// Mutation 目标根。
    target_root: &'a str,
    /// 受影响条目数。
    affected_count: usize,
    /// Apply 门禁结果。
    apply_allowed: bool,
    /// 是否规划 mutation。
    mutation_planned: bool,
    /// 是否执行 mutation。
    mutation_executed: bool,
    /// 是否要求回滚。
    rollback_required: bool,
    /// 是否已执行回滚。
    rollback_executed: bool,
    /// 下一步说明。
    next_action: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// CLI 可注入的恢复后健康结果，用于验证成功与失败/回滚路径。
pub(super) enum RestoreApplyHealthOption {
    /// 健康检查通过。
    Healthy,
    /// 健康检查失败及诊断消息。
    Failed(
        /// 健康检查失败时保留的诊断消息。
        String,
    ),
}

/// 解析 `restore plan|apply|rollback` 子命令。
pub(super) fn parse_restore_command(args: &[String]) -> Result<RestoreCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing restore subcommand"))?;
    match subcommand.as_str() {
        "plan" => Ok(RestoreCommand::Plan(parse_restore_plan_options(rest)?)),
        "apply" => Ok(RestoreCommand::Apply(parse_restore_apply_options(rest)?)),
        "rollback" => Ok(RestoreCommand::Rollback(parse_restore_rollback_options(
            rest,
        )?)),
        value => {
            Err(EvaError::unsupported("unknown restore subcommand")
                .with_context("subcommand", value))
        }
    }
}

/// 执行恢复命令并按变更/回滚结果选择退出码。
///
/// Dry-run 永不进入锁和 mutation 路径；apply 返回 rollback_required 或门禁禁止时使用
/// runtime-unavailable 退出码；rollback 只有明确 `rolled_back` 才返回成功。
pub(super) fn execute_restore<W, E>(
    command: RestoreCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        RestoreCommand::Plan(options) => {
            let trace = trace_for("cli.restore.plan");
            match create_restore_plan(&options) {
                Ok(result) => {
                    write_restore_plan(stdout, options.common.output, &result, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "restore.plan",
                    &error,
                    &trace,
                ),
            }
        }
        RestoreCommand::Apply(options) => {
            let trace = trace_for("cli.restore.apply");
            if options.dry_run {
                match create_restore_apply_dry_run(&options) {
                    Ok(result) => {
                        write_restore_apply_dry_run(
                            stdout,
                            options.common.output,
                            &result,
                            &trace,
                        )?;
                        Ok(EXIT_OK)
                    }
                    Err(error) => write_command_error(
                        stderr,
                        options.common.output,
                        "restore.apply",
                        &error,
                        &trace,
                    ),
                }
            } else {
                match create_restore_apply(&options, &trace) {
                    Ok(result) => {
                        let exit_code = if result
                            .mutation_apply
                            .as_ref()
                            .map(|report| report.rollback_required)
                            .unwrap_or(false)
                            || !result.report.apply_allowed
                        {
                            EXIT_RUNTIME_UNAVAILABLE
                        } else {
                            EXIT_OK
                        };
                        write_restore_apply(
                            stdout,
                            options.common.output,
                            exit_code,
                            &result,
                            &trace,
                        )?;
                        Ok(exit_code)
                    }
                    Err(error) => write_command_error(
                        stderr,
                        options.common.output,
                        "restore.apply",
                        &error,
                        &trace,
                    ),
                }
            }
        }
        RestoreCommand::Rollback(options) => {
            let trace = trace_for("cli.restore.rollback");
            match create_restore_rollback(&options, &trace) {
                Ok(result) => {
                    let exit_code = if result.rollback.status == "rolled_back" {
                        EXIT_OK
                    } else {
                        EXIT_RUNTIME_UNAVAILABLE
                    };
                    write_restore_rollback(
                        stdout,
                        options.common.output,
                        exit_code,
                        &result,
                        &trace,
                    )?;
                    Ok(exit_code)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "restore.rollback",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

/// 解析恢复计划的快照、请求、发布和 ArtifactStore 选项。
fn parse_restore_plan_options(args: &[String]) -> Result<RestorePlanOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut snapshot_id = "snapshot-v14".to_owned();
    let mut request_id = "req-restore-1".to_owned();
    let mut release_ref = "1.4.0".to_owned();
    let mut artifact_store = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--snapshot" | "--snapshot-id" => {
                index += 1;
                snapshot_id = required_option(args, index, "snapshot option")?.clone();
            }
            "--request-id" | "--task-id" | "--task" => {
                index += 1;
                request_id = required_option(args, index, "request id option")?.clone();
            }
            "--release" | "--release-ref" => {
                index += 1;
                release_ref = required_option(args, index, "release option")?.clone();
            }
            "--artifact-store" | "--artifact-store-dir" => {
                index += 1;
                artifact_store = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "artifact store option",
                )?));
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    RequestId::parse(&request_id)?;
    Ok(RestorePlanOptions {
        common: parse_common_options(&passthrough)?,
        snapshot_id,
        request_id,
        release_ref,
        artifact_store,
    })
}

/// 解析 apply 计划、确认、两类 store、owner、dry-run 和健康结果。
fn parse_restore_apply_options(args: &[String]) -> Result<RestoreApplyOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut plan = None;
    let mut confirm = None;
    let mut artifact_store = None;
    let mut lock_store = None;
    let mut dry_run = false;
    let mut owner = "cli".to_owned();
    let mut health = RestoreApplyHealthOption::Healthy;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--plan" => {
                index += 1;
                plan = Some(PathBuf::from(required_option(args, index, "plan option")?));
            }
            "--confirm" => {
                index += 1;
                confirm = Some(required_option(args, index, "confirm option")?.clone());
            }
            "--artifact-store" | "--artifact-store-dir" => {
                index += 1;
                artifact_store = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "artifact store option",
                )?));
            }
            "--lock-store" | "--lock-store-dir" => {
                index += 1;
                lock_store = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "lock store option",
                )?));
            }
            "--owner" => {
                index += 1;
                owner = required_option(args, index, "owner option")?.clone();
            }
            "--health" | "--health-check" => {
                index += 1;
                health =
                    parse_restore_apply_health(required_option(args, index, "health option")?)?;
            }
            "--dry-run" => {
                dry_run = true;
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    Ok(RestoreApplyOptions {
        common: parse_common_options(&passthrough)?,
        plan,
        confirm,
        artifact_store,
        lock_store,
        dry_run,
        owner,
        health,
    })
}

/// 解析 rollback 所需计划、确认、前置备份、锁和可选事务日志。
fn parse_restore_rollback_options(args: &[String]) -> Result<RestoreRollbackOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut plan = None;
    let mut confirm = None;
    let mut artifact_store = None;
    let mut lock_store = None;
    let mut transaction_log = None;
    let mut owner = "cli".to_owned();
    let mut health = RestoreApplyHealthOption::Healthy;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--plan" => {
                index += 1;
                plan = Some(PathBuf::from(required_option(args, index, "plan option")?));
            }
            "--confirm" => {
                index += 1;
                confirm = Some(required_option(args, index, "confirm option")?.clone());
            }
            "--artifact-store" | "--artifact-store-dir" => {
                index += 1;
                artifact_store = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "artifact store option",
                )?));
            }
            "--lock-store" | "--lock-store-dir" => {
                index += 1;
                lock_store = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "lock store option",
                )?));
            }
            "--transaction-log" | "--txn" => {
                index += 1;
                transaction_log = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "transaction log option",
                )?));
            }
            "--owner" => {
                index += 1;
                owner = required_option(args, index, "owner option")?.clone();
            }
            "--health" | "--health-check" => {
                index += 1;
                health =
                    parse_restore_apply_health(required_option(args, index, "health option")?)?;
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    Ok(RestoreRollbackOptions {
        common: parse_common_options(&passthrough)?,
        plan,
        confirm,
        artifact_store,
        lock_store,
        transaction_log,
        owner,
        health,
    })
}

/// 解析恢复健康检查的兼容别名，未知值作为参数错误返回。
fn parse_restore_apply_health(value: &str) -> Result<RestoreApplyHealthOption, EvaError> {
    match value {
        "healthy" | "pass" | "passed" => Ok(RestoreApplyHealthOption::Healthy),
        "failed" | "fail" | "unhealthy" => Ok(RestoreApplyHealthOption::Failed(
            "restore apply health check failed".to_owned(),
        )),
        value => Err(
            EvaError::invalid_argument("restore apply health must be healthy or failed")
                .with_context("health", value),
        ),
    }
}

/// 创建已验证备份和发布前快照，再由快照派生恢复计划。
/// 任一前置步骤失败都会阻止后续计划产生。
fn create_restore_plan(options: &RestorePlanOptions) -> Result<RestorePlanResult, EvaError> {
    let snapshot_options = snapshot_cmd::SnapshotCreateOptions {
        common: options.common.clone(),
        snapshot_id: options.snapshot_id.clone(),
        request_id: options.request_id.clone(),
        release_ref: options.release_ref.clone(),
        role: SnapshotRole::PreRelease,
        artifact_store: options.artifact_store.clone(),
    };
    let (backup, snapshot) = snapshot_cmd::create_snapshot_result(&snapshot_options)?;
    let plan = ReleaseSnapshotService.restore_plan(&snapshot);
    Ok(RestorePlanResult {
        backup,
        snapshot,
        plan,
    })
}

/// 验证 restore apply 的计划、确认令牌、目标备份和前置备份证据。
///
/// 这里不获取锁、不执行 mutation。确认不匹配按 PermissionDenied 返回；缺失产物按 NotFound
/// 返回；只有两份归档均存在且 digest/计划校验通过才产生 dry-run 报告。
fn create_restore_apply_dry_run(
    options: &RestoreApplyOptions,
) -> Result<RestoreApplyDryRunResult, EvaError> {
    let plan_path = options.plan.as_ref().ok_or_else(|| {
        EvaError::invalid_argument("restore apply dry-run requires --plan")
            .with_context("required_option", "--plan")
    })?;
    let confirm = options.confirm.as_deref().ok_or_else(|| {
        EvaError::invalid_argument("restore apply dry-run requires --confirm")
            .with_context("required_option", "--confirm")
    })?;
    let artifact_store_path = options.artifact_store.as_ref().ok_or_else(|| {
        EvaError::invalid_argument("restore apply dry-run requires --artifact-store")
            .with_context("required_option", "--artifact-store")
    })?;

    let plan = read_restore_apply_plan(plan_path)?;
    if confirm != plan.plan_id {
        return Err(EvaError::permission_denied(
            "restore apply confirmation does not match plan id",
        )
        .with_context("confirm", confirm)
        .with_context("plan_id", &plan.plan_id));
    }

    let store = FileSystemArtifactStore::new(artifact_store_path);
    let artifact_key = plan.backup_artifact_key();
    let artifact = store.try_get_bytes(&artifact_key)?.ok_or_else(|| {
        EvaError::not_found("restore apply backup artifact is missing")
            .with_context("artifact_key", &artifact_key)
            .with_context("artifact_store", artifact_store_path.display().to_string())
    })?;
    let pre_restore = plan.pre_restore_backup.as_ref().ok_or_else(|| {
        EvaError::invalid_argument("restore apply requires pre-restore backup evidence")
            .with_context("plan_id", &plan.plan_id)
            .with_context("required_field", "pre_restore_backup_artifact_id")
            .with_context("required_field", "pre_restore_backup_digest")
    })?;
    let pre_restore_key = pre_restore.backup_artifact_key();
    let pre_restore_artifact = store.try_get_bytes(&pre_restore_key)?.ok_or_else(|| {
        EvaError::not_found("restore apply pre-restore backup artifact is missing")
            .with_context("artifact_key", &pre_restore_key)
            .with_context("artifact_store", artifact_store_path.display().to_string())
    })?;
    let report = RestoreApplyValidator.dry_run(&plan, &artifact, Some(&pre_restore_artifact))?;

    Ok(RestoreApplyDryRunResult {
        plan,
        report,
        artifact_store: artifact_store_ref(Some(artifact_store_path)),
    })
}

/// 在 dry-run 证据之上执行策略、健康、锁和 staged file mutation 协调。
///
/// 顺序不可交换：先完整 dry-run，再加载策略并获取 apply 锁，只有 coordinator 返回
/// `apply_allowed` 且计划声明 mutation 才调用文件引擎。引擎部分失败会停止后续步骤、标记
/// rollback_required 并保留事务日志；健康失败另行生成代际 rollback plan。
fn create_restore_apply(
    options: &RestoreApplyOptions,
    trace: &TraceFields,
) -> Result<RestoreApplyResult, EvaError> {
    let lock_store_path = options.lock_store.as_ref().ok_or_else(|| {
        EvaError::invalid_argument("restore apply requires --lock-store")
            .with_context("required_option", "--lock-store")
    })?;
    let dry_run = create_restore_apply_dry_run(options)?;
    let project = load_project_config(&options.common.project_root)?;
    let policy_decision = RuntimePolicyGate::from_project(&project)?
        .decide(RuntimePolicyRequest::new(HighRiskAction::RestoreApply));
    let health = match &options.health {
        RestoreApplyHealthOption::Healthy => RestoreApplyHealthCheck::healthy(),
        RestoreApplyHealthOption::Failed(message) => {
            RestoreApplyHealthCheck::failed(message.clone())?
        }
    };
    let mut lock_store = FileSystemRestoreApplyLockStore::new(lock_store_path);
    let mut report = RestoreApplyCoordinator.apply(
        &mut lock_store,
        &dry_run.plan,
        &dry_run.report,
        &policy_decision,
        health,
        &options.owner,
    )?;
    let mutation_apply = if report.apply_allowed && report.mutation_plan.mutation_planned {
        let artifact_store_path = options.artifact_store.as_ref().ok_or_else(|| {
            EvaError::invalid_argument("restore mutation apply requires --artifact-store")
                .with_context("required_option", "--artifact-store")
        })?;
        let target_root = resolve_restore_mutation_target_root(
            &options.common.project_root,
            &report.mutation_plan.target_root,
        );
        let transaction_log_path = lock_store_path.join(format!("{}.restore.txn", report.plan_id));
        let sources = load_restore_mutation_sources(&dry_run.plan, artifact_store_path)?;
        let apply_report = RestoreMutationEngine.apply(
            &report.mutation_plan,
            &target_root,
            &transaction_log_path,
            &sources,
        )?;
        report.mutation_executed = apply_report.mutation_executed;
        if apply_report.rollback_required {
            report.status = "rollback_required".to_owned();
            report.apply_allowed = false;
            report.audit.extend(
                apply_report
                    .audit
                    .iter()
                    .map(|entry| format!("mutation:{entry}")),
            );
            report
                .risks
                .push("restore mutation stopped and requires rollback apply".to_owned());
        } else {
            report.status = "applied".to_owned();
            report.audit.extend(
                apply_report
                    .audit
                    .iter()
                    .map(|entry| format!("mutation:{entry}")),
            );
            report.steps.push("execute staged file mutation".to_owned());
        }
        Some(apply_report)
    } else {
        None
    };
    let rollback_plan = if report.health.healthy {
        None
    } else {
        Some(RollbackCoordinator.plan_failed_handoff(
            GenerationId::parse("gen-restore-apply")?,
            GenerationId::parse("gen-current")?,
            report.health.message.clone(),
            None,
        )?)
    };
    let rollback_required = mutation_apply
        .as_ref()
        .map(|report| report.rollback_required)
        .unwrap_or(false);
    record_restore_observability(
        &project,
        trace,
        RestoreObservabilityRecord {
            command: "restore.apply",
            plan_id: &report.plan_id,
            status: &report.status,
            apply_allowed: report.apply_allowed,
            mutation_executed: report.mutation_executed,
            rollback_required,
            rollback_executed: false,
        },
    );

    Ok(RestoreApplyResult {
        report,
        dry_run: dry_run.report,
        artifact_store: dry_run.artifact_store,
        lock_store: lock_store_ref(Some(lock_store_path)),
        mutation_apply,
        rollback_plan,
    })
}

/// 使用前置备份和 apply transaction log 执行 staged mutation 回滚。
///
/// 复用 dry-run 验证，随后要求策略允许和健康检查通过，再获取独占 rollback 锁。只有前置
/// 归档 digest 验证成功后才进入引擎；回滚审计聚合策略、归档和每个文件恢复步骤。
fn create_restore_rollback(
    options: &RestoreRollbackOptions,
    trace: &TraceFields,
) -> Result<RestoreRollbackResult, EvaError> {
    let lock_store_path = options.lock_store.as_ref().ok_or_else(|| {
        EvaError::invalid_argument("restore rollback requires --lock-store")
            .with_context("required_option", "--lock-store")
    })?;
    let dry_run = create_restore_apply_dry_run(&RestoreApplyOptions {
        common: options.common.clone(),
        plan: options.plan.clone(),
        confirm: options.confirm.clone(),
        artifact_store: options.artifact_store.clone(),
        lock_store: options.lock_store.clone(),
        dry_run: true,
        owner: options.owner.clone(),
        health: options.health.clone(),
    })?;
    let project = load_project_config(&options.common.project_root)?;
    let policy_decision = RuntimePolicyGate::from_project(&project)?
        .decide(RuntimePolicyRequest::new(HighRiskAction::RestoreApply));
    policy_decision.ensure_allowed()?;
    let health = match &options.health {
        RestoreApplyHealthOption::Healthy => RestoreApplyHealthCheck::healthy(),
        RestoreApplyHealthOption::Failed(message) => {
            RestoreApplyHealthCheck::failed(message.clone())?
        }
    };
    if !health.healthy {
        return Err(
            EvaError::unavailable("restore rollback health check failed")
                .with_context("health", health.message.clone()),
        );
    }
    let mut lock_store = FileSystemRestoreApplyLockStore::new(lock_store_path);
    let lock = lock_store.acquire_rollback_lock(&dry_run.plan, &options.owner)?;
    let artifact_store_path = options.artifact_store.as_ref().ok_or_else(|| {
        EvaError::invalid_argument("restore rollback requires --artifact-store")
            .with_context("required_option", "--artifact-store")
    })?;
    let pre_restore = dry_run.plan.pre_restore_backup.as_ref().ok_or_else(|| {
        EvaError::invalid_argument("restore rollback requires pre-restore backup evidence")
            .with_context("plan_id", &dry_run.plan.plan_id)
    })?;
    let artifact_store = FileSystemArtifactStore::new(artifact_store_path);
    let pre_restore_artifact = artifact_store
        .try_get_bytes(&pre_restore.backup_artifact_key())?
        .ok_or_else(|| {
            EvaError::not_found("restore rollback pre-restore backup artifact is missing")
                .with_context("artifact_key", pre_restore.backup_artifact_key())
                .with_context("artifact_store", artifact_store_path.display().to_string())
        })?;
    let pre_restore_archive =
        RestorePreRestoreArchive::parse(&pre_restore_artifact, &pre_restore.backup_digest)?;
    let target_root = resolve_restore_mutation_target_root(
        &options.common.project_root,
        &dry_run.report.mutation_plan.target_root,
    );
    let transaction_log_path = options
        .transaction_log
        .clone()
        .unwrap_or_else(|| lock_store_path.join(format!("{}.restore.txn", dry_run.plan.plan_id)));
    let rollback_log_path =
        lock_store_path.join(format!("{}.restore.rollback.txn", dry_run.plan.plan_id));
    let rollback = RestoreRollbackEngine.apply(
        &dry_run.report.mutation_plan,
        &target_root,
        &transaction_log_path,
        &rollback_log_path,
        &pre_restore_archive,
    )?;
    let mut audit = vec![
        "restore.rollback:plan_parsed".to_owned(),
        "restore.rollback:confirmation_matched".to_owned(),
        "restore.rollback:backup_evidence_verified".to_owned(),
        "restore.rollback:policy_allowed".to_owned(),
        "restore.rollback:lock_acquired".to_owned(),
        "restore.rollback:health_check_passed".to_owned(),
    ];
    audit.extend(
        policy_decision
            .audit
            .iter()
            .map(|entry| format!("policy:{entry}")),
    );
    audit.extend(
        pre_restore_archive
            .audit
            .iter()
            .map(|entry| format!("pre_restore:{entry}")),
    );
    audit.extend(
        rollback
            .audit
            .iter()
            .map(|entry| format!("rollback:{entry}")),
    );
    record_restore_observability(
        &project,
        trace,
        RestoreObservabilityRecord {
            command: "restore.rollback",
            plan_id: &dry_run.plan.plan_id,
            status: &rollback.status,
            apply_allowed: true,
            mutation_executed: false,
            rollback_required: rollback.status != "rolled_back",
            rollback_executed: rollback.rollback_executed,
        },
    );
    Ok(RestoreRollbackResult {
        plan: dry_run.plan,
        dry_run: dry_run.report,
        rollback,
        lock,
        health,
        artifact_store: dry_run.artifact_store,
        lock_store: lock_store_ref(Some(lock_store_path)),
        audit,
    })
}

/// Restore 边界写入 best-effort 可观测性 pipeline 的最小事实集合。
struct RestoreObservabilityRecord<'a> {
    /// restore.apply 或 restore.rollback。
    command: &'a str,
    /// 关联计划 ID。
    plan_id: &'a str,
    /// 最终状态。
    status: &'a str,
    /// Apply 门禁结果。
    apply_allowed: bool,
    /// 文件 mutation 是否执行。
    mutation_executed: bool,
    /// 是否需要后续回滚。
    rollback_required: bool,
    /// 文件回滚是否执行。
    rollback_executed: bool,
}

/// 以 best-effort 语义记录 restore audit、metric 和 span。
/// 可观测性故障不得覆盖已完成的恢复/回滚结果，因此本函数吞掉 sink 错误并且不返回 Result。
fn record_restore_observability(
    project: &ProjectConfig,
    trace: &TraceFields,
    record: RestoreObservabilityRecord<'_>,
) {
    let backend = restore_observability_backend(project);
    let (action, span_name) = if record.command == "restore.rollback" {
        (AuditAction::RestoreRollback, "runtime.restore.rollback")
    } else {
        (AuditAction::RestoreApply, "runtime.restore.apply")
    };
    let Ok(span_id) = SpanId::parse(span_name) else {
        return;
    };
    let mut pipeline = BestEffortObservabilityPipeline::open(backend);
    let observed_trace = trace.child_span(span_id);
    let outcome = if record.rollback_required || record.status == "rollback_failed" {
        AuditOutcome::Failed
    } else if !record.apply_allowed && !record.rollback_executed {
        AuditOutcome::Blocked
    } else {
        AuditOutcome::Ok
    };
    let _ = AuditSink::record(
        &mut pipeline,
        AuditEvent::new(action, outcome, observed_trace.clone())
            .with_message("restore boundary observed")
            .with_field("command", record.command)
            .with_field("plan_id", record.plan_id)
            .with_field("status", record.status)
            .with_field("apply_allowed", record.apply_allowed.to_string())
            .with_field("mutation_executed", record.mutation_executed.to_string())
            .with_field("rollback_required", record.rollback_required.to_string())
            .with_field("rollback_executed", record.rollback_executed.to_string()),
    );
    if let Ok(name) = MetricName::parse(span_name) {
        let _ = MetricSink::record(
            &mut pipeline,
            MetricPoint::new(name, MetricKind::Counter, 1.0).with_labels(
                MetricLabels::runtime("cli_restore_v1.16.1", "restore")
                    .with("command", record.command)
                    .with("status", record.status)
                    .with("mutation_executed", record.mutation_executed.to_string())
                    .with("rollback_executed", record.rollback_executed.to_string()),
            ),
        );
    }
    let _ = pipeline.export_span(
        span_name,
        &observed_trace,
        &[
            ("component", "cli"),
            ("command", record.command),
            ("status", record.status),
        ],
    );
}

/// 从项目 data_dir 推导 restore 可观测性目录，并正确解析相对路径。
fn restore_observability_backend(project: &ProjectConfig) -> PathBuf {
    let data_dir = project
        .eva
        .runtime
        .data_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(".eva/data"));
    if data_dir.is_absolute() {
        data_dir.join("observability")
    } else {
        project.project_root.join(data_dir).join("observability")
    }
}

/// 将计划中的 mutation target root 解析为绝对路径；相对值以项目根为基准。
fn resolve_restore_mutation_target_root(project_root: &Path, target_root: &str) -> PathBuf {
    let target = PathBuf::from(target_root);
    if target.is_absolute() {
        target
    } else {
        project_root.join(target)
    }
}

/// 预加载 mutation 步骤引用的唯一源产物。
/// 缺少任何 source 时在文件写入前失败，避免执行中途才发现不完整输入。
fn load_restore_mutation_sources(
    plan: &RestoreApplyPlan,
    artifact_store_path: &Path,
) -> Result<BTreeMap<String, eva_storage::ArtifactRecord>, EvaError> {
    let store = FileSystemArtifactStore::new(artifact_store_path);
    let mut sources = BTreeMap::new();
    for step in &plan.mutation_steps {
        let Some(key) = &step.source_artifact_key else {
            continue;
        };
        if sources.contains_key(key) {
            continue;
        }
        let artifact = store.try_get_bytes(key)?.ok_or_else(|| {
            EvaError::not_found("restore mutation source artifact is missing")
                .with_context("artifact_key", key)
                .with_context("artifact_store", artifact_store_path.display().to_string())
        })?;
        sources.insert(key.clone(), artifact);
    }
    Ok(sources)
}

/// 读取并解析 restore apply 计划文件，并在所有失败上附加路径上下文。
fn read_restore_apply_plan(path: &Path) -> Result<RestoreApplyPlan, EvaError> {
    let data = fs::read_to_string(path).map_err(|error| {
        let message = if error.kind() == std::io::ErrorKind::NotFound {
            "restore apply plan is missing"
        } else {
            "failed to read restore apply plan"
        };
        EvaError::not_found(message)
            .with_context("plan", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    parse_restore_apply_plan(&data)
        .map_err(|error| error.with_context("plan", path.display().to_string()))
}

/// 解析稳定的逐行 restore apply plan 格式。
///
/// 解析器允许 UTF-8 BOM 并拒绝缺失必填字段、未知 mutation 操作和不完整前置备份证据；
/// 结构化 API 构造器负责最终跨字段校验，防止文本入口绕过领域约束。
pub(super) fn parse_restore_apply_plan(data: &str) -> Result<RestoreApplyPlan, EvaError> {
    let mut plan_id = None;
    let mut backup_artifact_id = None;
    let mut backup_digest = None;
    let mut pre_restore_backup_artifact_id = None;
    let mut pre_restore_backup_digest = None;
    let mut mutation_target_root = None;
    let mut mutation_steps = Vec::new();
    for line in data.lines() {
        let line = line.trim_start_matches('\u{feff}');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            return Err(EvaError::invalid_argument(
                "restore apply plan line must use key=value format",
            ));
        };
        match key {
            "plan_id" => plan_id = Some(value.to_owned()),
            "backup_artifact_id" => backup_artifact_id = Some(value.to_owned()),
            "backup_digest" => backup_digest = Some(value.to_owned()),
            "pre_restore_backup_artifact_id" => {
                pre_restore_backup_artifact_id = Some(value.to_owned())
            }
            "pre_restore_backup_digest" => pre_restore_backup_digest = Some(value.to_owned()),
            "restore_target_root" | "mutation_target_root" => {
                mutation_target_root = Some(value.to_owned())
            }
            "mutation_step" => mutation_steps.push(parse_restore_mutation_step(value)?),
            _ => {
                return Err(EvaError::invalid_argument(
                    "restore apply plan contains unsupported field",
                )
                .with_context("field", key));
            }
        }
    }
    let plan = RestoreApplyPlan::new(
        plan_id.ok_or_else(|| EvaError::invalid_argument("restore apply plan missing plan_id"))?,
        backup_artifact_id.ok_or_else(|| {
            EvaError::invalid_argument("restore apply plan missing backup_artifact_id")
        })?,
        backup_digest.ok_or_else(|| {
            EvaError::invalid_argument("restore apply plan missing backup_digest")
        })?,
    )?;
    let mut plan = match (pre_restore_backup_artifact_id, pre_restore_backup_digest) {
        (Some(artifact_id), Some(digest)) => {
            plan.with_pre_restore_backup(PreRestoreBackupEvidence::new(artifact_id, digest)?)
        }
        (None, None) => plan,
        _ => Err(EvaError::invalid_argument(
            "restore apply plan pre-restore backup evidence must include artifact id and digest",
        ))?,
    };
    if let Some(target_root) = mutation_target_root {
        plan = plan.with_mutation_target_root(target_root)?;
    }
    if !mutation_steps.is_empty() {
        plan = plan.with_mutation_steps(mutation_steps);
    }
    Ok(plan)
}

/// 解析单条 staged mutation 步骤及其目标、来源和预期 digest。
fn parse_restore_mutation_step(value: &str) -> Result<RestoreMutationStep, EvaError> {
    let parts = value.split('|').collect::<Vec<_>>();
    if parts.len() != 6 {
        return Err(EvaError::invalid_argument(
            "restore mutation_step must use operation|relative_path|source_artifact_key|expected_digest|pre_restore_digest|target_kind format",
        )
        .with_context("mutation_step", value));
    }
    let operation = RestoreMutationOperation::parse(parts[0])?;
    let target_kind = RestoreMutationTargetKind::parse(parts[5])?;
    RestoreMutationStep::new(
        operation,
        parts[1],
        optional_mutation_field(parts[2]),
        optional_mutation_field(parts[3]),
        optional_mutation_field(parts[4]),
        target_kind,
    )
}

/// 将 mutation 文本格式中的空值规范为 None。
fn optional_mutation_field(value: &str) -> Option<String> {
    match value {
        "" | "-" | "none" | "null" => None,
        _ => Some(value.to_owned()),
    }
}

/// 输出恢复计划及其前置备份和快照证据。
fn write_restore_plan<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    result: &RestorePlanResult,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Restore plan").map_err(write_error_kind)?;
            writeln!(writer, "snapshot: {}", result.snapshot.snapshot_id)
                .map_err(write_error_kind)?;
            writeln!(writer, "status: {}", result.plan.status).map_err(write_error_kind)?;
            writeln!(writer, "apply_allowed: {}", result.plan.apply_allowed)
                .map_err(write_error_kind)?;
            writeln!(writer, "mutation_executed: false").map_err(write_error_kind)?;
            write_artifact_store_ref(writer, &result.backup.artifact_store)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("restore.plan", EXIT_OK, &restore_plan_json(result), trace)
        )
        .map_err(write_error_kind),
    }
}

/// 输出 dry-run 验证、mutation 计划、风险和操作者确认摘要。
fn write_restore_apply_dry_run<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    result: &RestoreApplyDryRunResult,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Restore apply dry-run").map_err(write_error_kind)?;
            writeln!(writer, "plan: {}", result.report.plan_id).map_err(write_error_kind)?;
            writeln!(writer, "status: {}", result.report.status).map_err(write_error_kind)?;
            writeln!(writer, "apply_allowed: {}", result.report.apply_allowed)
                .map_err(write_error_kind)?;
            writeln!(writer, "mutation_executed: false").map_err(write_error_kind)?;
            writeln!(
                writer,
                "mutation_planned: {}",
                result.report.mutation_plan.mutation_planned
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "affected_paths: {}",
                result.report.mutation_plan.affected_paths.len()
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "preflight_hash: {}",
                result.report.mutation_plan.preflight_hash
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "pre_restore_backup: {}",
                result.report.pre_restore_backup_artifact_key
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "final_state: {}", result.report.status).map_err(write_error_kind)?;
            writeln!(
                writer,
                "rollback_path: {}",
                restore_apply_dry_run_rollback_path(&result.report)
            )
            .map_err(write_error_kind)?;
            let risks = restore_apply_dry_run_risks(&result.report);
            write_risk_lines_text(writer, &risks)?;
            write_restore_operator_confirmation_text(
                writer,
                &restore_apply_dry_run_operator_confirmation(result),
            )?;
            write_artifact_store_ref(writer, &result.artifact_store)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "restore.apply",
                EXIT_OK,
                &restore_apply_dry_run_json(result),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

/// 输出 apply 协调器、文件事务、健康和可选回滚计划结果。
fn write_restore_apply<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    exit_code: i32,
    result: &RestoreApplyResult,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Restore apply gate").map_err(write_error_kind)?;
            writeln!(writer, "plan: {}", result.report.plan_id).map_err(write_error_kind)?;
            writeln!(writer, "status: {}", result.report.status).map_err(write_error_kind)?;
            writeln!(writer, "apply_allowed: {}", result.report.apply_allowed)
                .map_err(write_error_kind)?;
            writeln!(
                writer,
                "mutation_executed: {}",
                result.report.mutation_executed
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "mutation_planned: {}",
                result.report.mutation_plan.mutation_planned
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "preflight_hash: {}",
                result.report.mutation_plan.preflight_hash
            )
            .map_err(write_error_kind)?;
            if let Some(mutation_apply) = &result.mutation_apply {
                writeln!(writer, "mutation_status: {}", mutation_apply.status)
                    .map_err(write_error_kind)?;
                writeln!(
                    writer,
                    "rollback_required: {}",
                    mutation_apply.rollback_required
                )
                .map_err(write_error_kind)?;
                writeln!(
                    writer,
                    "transaction_log: {}",
                    mutation_apply.transaction_log_path
                )
                .map_err(write_error_kind)?;
            }
            writeln!(writer, "lock: {}", result.report.lock.lock_id).map_err(write_error_kind)?;
            writeln!(writer, "health: {}", result.report.health.message)
                .map_err(write_error_kind)?;
            if let Some(rollback) = &result.rollback_plan {
                writeln!(writer, "rollback: {}", rollback.status).map_err(write_error_kind)?;
            }
            writeln!(writer, "final_state: {}", restore_apply_final_state(result))
                .map_err(write_error_kind)?;
            writeln!(
                writer,
                "rollback_path: {}",
                restore_apply_rollback_path(result)
            )
            .map_err(write_error_kind)?;
            write_risk_lines_text(writer, &result.report.risks)?;
            write_restore_operator_confirmation_text(
                writer,
                &restore_apply_operator_confirmation(result),
            )?;
            write_artifact_store_ref(writer, &result.artifact_store)?;
            write_lock_store_ref(writer, &result.lock_store)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "restore.apply",
                exit_code,
                &restore_apply_json(result),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

/// 输出文件回滚结果、锁、健康、审计和操作者确认摘要。
fn write_restore_rollback<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    exit_code: i32,
    result: &RestoreRollbackResult,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Restore rollback apply").map_err(write_error_kind)?;
            writeln!(writer, "plan: {}", result.plan.plan_id).map_err(write_error_kind)?;
            writeln!(writer, "status: {}", result.rollback.status).map_err(write_error_kind)?;
            writeln!(
                writer,
                "mutation_executed: {}",
                result.rollback.rollback_executed
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "rollback_executed: {}",
                result.rollback.rollback_executed
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "transaction_log: {}",
                result.rollback.transaction_log_path
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "rollback_log: {}",
                result.rollback.rollback_log_path
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "final_state: {}", result.rollback.status)
                .map_err(write_error_kind)?;
            writeln!(writer, "rollback_path: {}", restore_rollback_path(result))
                .map_err(write_error_kind)?;
            let risks = restore_rollback_risks(result);
            write_risk_lines_text(writer, &risks)?;
            writeln!(writer, "lock: {}", result.lock.lock_id).map_err(write_error_kind)?;
            write_restore_operator_confirmation_text(
                writer,
                &restore_rollback_operator_confirmation(result),
            )?;
            write_artifact_store_ref(writer, &result.artifact_store)?;
            write_lock_store_ref(writer, &result.lock_store)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "restore.rollback",
                exit_code,
                &restore_rollback_json(result),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

/// 根据 dry-run mutation 计划描述预期回滚路径。
fn restore_apply_dry_run_rollback_path(report: &RestoreApplyDryRunReport) -> String {
    format!(
        "pre_restore_backup={};rollback_manifest_entries={}",
        report.pre_restore_backup_artifact_key,
        report.mutation_plan.rollback_manifest.len()
    )
}

/// 汇总 dry-run 原始风险并补充 mutation/回滚操作风险。
fn restore_apply_dry_run_risks(report: &RestoreApplyDryRunReport) -> Vec<String> {
    if report.mutation_plan.mutation_planned {
        vec![
            format!(
                "{} affected paths require review before confirmation",
                report.mutation_plan.affected_paths.len()
            ),
            "destructive restore can overwrite or delete files".to_owned(),
        ]
    } else {
        vec!["no staged mutation steps were declared; apply remains gated".to_owned()]
    }
}

/// 依据 mutation 与 coordinator 结果选择面向操作者的最终状态。
fn restore_apply_final_state(result: &RestoreApplyResult) -> &str {
    result
        .mutation_apply
        .as_ref()
        .map(|apply| apply.status.as_str())
        .unwrap_or(result.report.status.as_str())
}

/// 描述 apply 结果对应的文件或代际回滚路径。
fn restore_apply_rollback_path(result: &RestoreApplyResult) -> String {
    if let Some(rollback) = &result.rollback_plan {
        return format!(
            "rollback_plan:{}:{}->{}",
            rollback.status,
            rollback.from_generation.as_str(),
            rollback.to_generation.as_str()
        );
    }
    if let Some(mutation_apply) = &result.mutation_apply {
        return format!("transaction_log={}", mutation_apply.transaction_log_path);
    }
    "none; no mutation executed".to_owned()
}

/// 描述 rollback 结果及其日志/前置备份路径。
fn restore_rollback_path(result: &RestoreRollbackResult) -> String {
    format!(
        "transaction_log={};rollback_log={}",
        result.rollback.transaction_log_path, result.rollback.rollback_log_path
    )
}

/// 汇总 rollback 引擎风险和未完全恢复时的后续风险。
fn restore_rollback_risks(result: &RestoreRollbackResult) -> Vec<String> {
    let mut risks = vec!["rollback rewrites target files from pre-restore archive".to_owned()];
    if !result.rollback.rollback_executed {
        risks.push("manual recovery may be required before retrying".to_owned());
    }
    risks
}

/// 将恢复计划、快照和前置备份编码为 JSON。
fn restore_plan_json(result: &RestorePlanResult) -> String {
    format!(
        "{{\"snapshot\":{},\"backup\":{},\"plan\":{{\"snapshot_id\":{},\"status\":{},\"apply_allowed\":{},\"mutation_executed\":false,\"steps\":{},\"risks\":{},\"audit\":{}}}}}",
        snapshot_cmd::snapshot_json(&result.snapshot),
        backup_cmd::backup_result_json(&result.backup),
        json_string(&result.plan.snapshot_id),
        json_string(&result.plan.status),
        result.plan.apply_allowed,
        json_array(result.plan.steps.iter().map(|step| json_string(step))),
        json_array(result.plan.risks.iter().map(|risk| json_string(risk))),
        json_array(result.plan.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将 dry-run 结果和 ArtifactStore 描述编码为 JSON。
fn restore_apply_dry_run_json(result: &RestoreApplyDryRunResult) -> String {
    restore_apply_dry_run_report_json(&result.report, &result.artifact_store)
}

/// 将 dry-run 验证报告、mutation 计划与确认摘要编码为 JSON。
fn restore_apply_dry_run_report_json(
    report: &RestoreApplyDryRunReport,
    artifact_store: &ArtifactStoreRef,
) -> String {
    format!(
        "{{\"plan_id\":{},\"status\":{},\"apply_allowed\":{},\"mutation_executed\":false,\"backup_artifact_key\":{},\"expected_digest\":{},\"actual_digest\":{},\"pre_restore_backup_artifact_key\":{},\"pre_restore_expected_digest\":{},\"pre_restore_actual_digest\":{},\"mutation_plan\":{},\"operator_confirmation\":{},\"artifact_store\":{},\"audit\":{}}}",
        json_string(&report.plan_id),
        json_string(&report.status),
        report.apply_allowed,
        json_string(&report.backup_artifact_key),
        json_string(&report.expected_digest),
        json_string(&report.actual_digest),
        json_string(&report.pre_restore_backup_artifact_key),
        json_string(&report.pre_restore_expected_digest),
        json_string(&report.pre_restore_actual_digest),
        restore_mutation_plan_json(&report.mutation_plan),
        restore_operator_confirmation_json(&restore_apply_dry_run_report_operator_confirmation(
            report,
        )),
        artifact_store_ref_json(artifact_store),
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将 restore apply 的门禁、锁、事务、回滚与确认事实编码为 JSON。
fn restore_apply_json(result: &RestoreApplyResult) -> String {
    format!(
        "{{\"plan_id\":{},\"status\":{},\"apply_allowed\":{},\"mutation_executed\":{},\"mutation_plan\":{},\"mutation_apply\":{},\"operator_confirmation\":{},\"backup_artifact_key\":{},\"pre_restore_backup_artifact_key\":{},\"lock\":{},\"health\":{{\"healthy\":{},\"message\":{}}},\"artifact_store\":{},\"lock_store\":{},\"dry_run\":{},\"steps\":{},\"risks\":{},\"audit\":{},\"rollback_plan\":{}}}",
        json_string(&result.report.plan_id),
        json_string(&result.report.status),
        result.report.apply_allowed,
        result.report.mutation_executed,
        restore_mutation_plan_json(&result.report.mutation_plan),
        result
            .mutation_apply
            .as_ref()
            .map(restore_mutation_apply_json)
            .unwrap_or_else(|| "null".to_owned()),
        restore_operator_confirmation_json(&restore_apply_operator_confirmation(result)),
        json_string(&result.report.backup_artifact_key),
        json_string(&result.report.pre_restore_backup_artifact_key),
        restore_apply_lock_json(&result.report.lock),
        result.report.health.healthy,
        json_string(&result.report.health.message),
        artifact_store_ref_json(&result.artifact_store),
        lock_store_ref_json(&result.lock_store),
        restore_apply_dry_run_report_json(&result.dry_run, &result.artifact_store),
        json_array(result.report.steps.iter().map(|step| json_string(step))),
        json_array(result.report.risks.iter().map(|risk| json_string(risk))),
        json_array(result.report.audit.iter().map(|entry| json_string(entry))),
        result
            .rollback_plan
            .as_ref()
            .map(rollback_plan_json)
            .unwrap_or_else(|| "null".to_owned())
    )
}

/// 将 restore rollback 的证据、锁、健康和审计编码为 JSON。
fn restore_rollback_json(result: &RestoreRollbackResult) -> String {
    format!(
        "{{\"plan_id\":{},\"status\":{},\"mutation_executed\":{},\"rollback_executed\":{},\"rollback\":{},\"mutation_plan\":{},\"operator_confirmation\":{},\"lock\":{},\"health\":{{\"healthy\":{},\"message\":{}}},\"artifact_store\":{},\"lock_store\":{},\"dry_run\":{},\"audit\":{}}}",
        json_string(&result.plan.plan_id),
        json_string(&result.rollback.status),
        result.rollback.rollback_executed,
        result.rollback.rollback_executed,
        restore_rollback_apply_json(&result.rollback),
        restore_mutation_plan_json(&result.dry_run.mutation_plan),
        restore_operator_confirmation_json(&restore_rollback_operator_confirmation(result)),
        restore_apply_lock_json(&result.lock),
        result.health.healthy,
        json_string(&result.health.message),
        artifact_store_ref_json(&result.artifact_store),
        lock_store_ref_json(&result.lock_store),
        restore_apply_dry_run_report_json(&result.dry_run, &result.artifact_store),
        json_array(result.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将文件回滚引擎报告编码为 JSON。
fn restore_rollback_apply_json(report: &RestoreRollbackApplyReport) -> String {
    format!(
        "{{\"plan_id\":{},\"target_root\":{},\"status\":{},\"rollback_executed\":{},\"completed_steps\":{},\"failed_step\":{},\"transaction_log_path\":{},\"rollback_log_path\":{},\"transaction_status\":{},\"transaction_log\":{},\"rollback_log\":{},\"audit\":{}}}",
        json_string(&report.plan_id),
        json_string(&report.target_root),
        json_string(&report.status),
        report.rollback_executed,
        report.completed_steps,
        option_json(report.failed_step.as_deref()),
        json_string(&report.transaction_log_path),
        json_string(&report.rollback_log_path),
        json_string(&report.transaction_status),
        json_array(report.transaction_log.iter().map(restore_transaction_entry_json)),
        json_array(report.rollback_log.iter().map(restore_transaction_entry_json)),
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 从 dry-run 组合结果构造统一操作者确认摘要。
fn restore_apply_dry_run_operator_confirmation(
    result: &RestoreApplyDryRunResult,
) -> RestoreOperatorConfirmation {
    restore_apply_dry_run_report_operator_confirmation(&result.report)
}

/// 从 dry-run 报告构造尚未执行 mutation 的确认摘要。
fn restore_apply_dry_run_report_operator_confirmation(
    report: &RestoreApplyDryRunReport,
) -> RestoreOperatorConfirmation {
    restore_operator_confirmation(RestoreOperatorConfirmationInput {
        command: "restore.apply.dry_run",
        plan_id: &report.plan_id,
        target_root: &report.mutation_plan.target_root,
        affected_count: report.mutation_plan.affected_paths.len(),
        apply_allowed: report.apply_allowed,
        mutation_planned: report.mutation_plan.mutation_planned,
        mutation_executed: false,
        rollback_required: false,
        rollback_executed: false,
        next_action:
            "dry-run only; rerun restore apply with matching --confirm after reviewing affected paths",
    })
}

/// 从真实 apply 结果构造包含执行和回滚事实的确认摘要。
fn restore_apply_operator_confirmation(result: &RestoreApplyResult) -> RestoreOperatorConfirmation {
    let rollback_required = result
        .mutation_apply
        .as_ref()
        .map(|report| report.rollback_required)
        .unwrap_or(!result.report.health.healthy);
    let next_action = if rollback_required {
        "run restore rollback with the same plan id after inspecting the transaction log"
    } else if result.report.mutation_executed {
        "verify restored files and keep the transaction log for audit"
    } else if result.report.apply_allowed && result.report.mutation_plan.mutation_planned {
        "review staged mutation plan before enabling destructive execution"
    } else {
        "no file mutation executed; review mutation_plan before retrying with staged steps"
    };
    restore_operator_confirmation(RestoreOperatorConfirmationInput {
        command: "restore.apply",
        plan_id: &result.report.plan_id,
        target_root: &result.report.mutation_plan.target_root,
        affected_count: result.report.mutation_plan.affected_paths.len(),
        apply_allowed: result.report.apply_allowed,
        mutation_planned: result.report.mutation_plan.mutation_planned,
        mutation_executed: result.report.mutation_executed,
        rollback_required,
        rollback_executed: false,
        next_action,
    })
}

/// 从 rollback 结果构造确认摘要，突出 rollback_executed 事实。
fn restore_rollback_operator_confirmation(
    result: &RestoreRollbackResult,
) -> RestoreOperatorConfirmation {
    let next_action = if result.rollback.rollback_executed {
        "verify restored pre-restore bytes and keep rollback log for audit"
    } else {
        "inspect rollback log and resolve manual recovery before retrying"
    };
    restore_operator_confirmation(RestoreOperatorConfirmationInput {
        command: "restore.rollback",
        plan_id: &result.plan.plan_id,
        target_root: &result.dry_run.mutation_plan.target_root,
        affected_count: result.dry_run.mutation_plan.affected_paths.len(),
        apply_allowed: true,
        mutation_planned: result.dry_run.mutation_plan.mutation_planned,
        mutation_executed: false,
        rollback_required: result.rollback.status != "rolled_back",
        rollback_executed: result.rollback.rollback_executed,
        next_action,
    })
}

/// 集中构造 restore 操作者确认契约，保证 apply/dry-run/rollback 字段语义一致。
fn restore_operator_confirmation(
    input: RestoreOperatorConfirmationInput<'_>,
) -> RestoreOperatorConfirmation {
    RestoreOperatorConfirmation {
        command: input.command.to_owned(),
        plan_id: input.plan_id.to_owned(),
        confirm_token: input.plan_id.to_owned(),
        target_root: input.target_root.to_owned(),
        affected_count: input.affected_count,
        apply_allowed: input.apply_allowed,
        mutation_planned: input.mutation_planned,
        mutation_executed: input.mutation_executed,
        rollback_required: input.rollback_required,
        rollback_executed: input.rollback_executed,
        irreversible_warning:
            "destructive restore can overwrite or delete files; proceed only after reviewing plan id, target root, affected count, and rollback evidence".to_owned(),
        next_action: input.next_action.to_owned(),
    }
}

/// 将操作者确认摘要编码为 JSON。
fn restore_operator_confirmation_json(confirmation: &RestoreOperatorConfirmation) -> String {
    format!(
        "{{\"command\":{},\"plan_id\":{},\"confirm_token\":{},\"target_root\":{},\"affected_count\":{},\"apply_allowed\":{},\"mutation_planned\":{},\"mutation_executed\":{},\"rollback_required\":{},\"rollback_executed\":{},\"irreversible_warning\":{},\"next_action\":{}}}",
        json_string(&confirmation.command),
        json_string(&confirmation.plan_id),
        json_string(&confirmation.confirm_token),
        json_string(&confirmation.target_root),
        confirmation.affected_count,
        confirmation.apply_allowed,
        confirmation.mutation_planned,
        confirmation.mutation_executed,
        confirmation.rollback_required,
        confirmation.rollback_executed,
        json_string(&confirmation.irreversible_warning),
        json_string(&confirmation.next_action)
    )
}

/// 以稳定逐行字段写出操作者确认摘要。
fn write_restore_operator_confirmation_text<W: Write>(
    writer: &mut W,
    confirmation: &RestoreOperatorConfirmation,
) -> Result<(), EvaError> {
    writeln!(writer, "operator_confirmation: {}", confirmation.command)
        .map_err(write_error_kind)?;
    writeln!(writer, "plan_id: {}", confirmation.plan_id).map_err(write_error_kind)?;
    writeln!(writer, "confirm_token: {}", confirmation.confirm_token).map_err(write_error_kind)?;
    writeln!(writer, "target_root: {}", confirmation.target_root).map_err(write_error_kind)?;
    writeln!(writer, "affected_count: {}", confirmation.affected_count)
        .map_err(write_error_kind)?;
    writeln!(writer, "apply_allowed: {}", confirmation.apply_allowed).map_err(write_error_kind)?;
    writeln!(
        writer,
        "mutation_planned: {}",
        confirmation.mutation_planned
    )
    .map_err(write_error_kind)?;
    writeln!(
        writer,
        "mutation_executed: {}",
        confirmation.mutation_executed
    )
    .map_err(write_error_kind)?;
    writeln!(
        writer,
        "rollback_required: {}",
        confirmation.rollback_required
    )
    .map_err(write_error_kind)?;
    writeln!(
        writer,
        "rollback_executed: {}",
        confirmation.rollback_executed
    )
    .map_err(write_error_kind)?;
    writeln!(
        writer,
        "irreversible_warning: {}",
        confirmation.irreversible_warning
    )
    .map_err(write_error_kind)?;
    writeln!(writer, "next_action: {}", confirmation.next_action).map_err(write_error_kind)
}

/// 将 staged mutation 事务结果编码为 JSON。
fn restore_mutation_apply_json(report: &RestoreMutationApplyReport) -> String {
    format!(
        "{{\"plan_id\":{},\"target_root\":{},\"status\":{},\"mutation_executed\":{},\"rollback_required\":{},\"completed_steps\":{},\"failed_step\":{},\"transaction_log_path\":{},\"transaction_log\":{},\"audit\":{}}}",
        json_string(&report.plan_id),
        json_string(&report.target_root),
        json_string(&report.status),
        report.mutation_executed,
        report.rollback_required,
        report.completed_steps,
        option_json(report.failed_step.as_deref()),
        json_string(&report.transaction_log_path),
        json_array(report.transaction_log.iter().map(restore_transaction_entry_json)),
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将单个 mutation 事务条目编码为 JSON。
fn restore_transaction_entry_json(entry: &RestoreMutationTransactionEntry) -> String {
    format!(
        "{{\"sequence\":{},\"operation\":{},\"relative_path\":{},\"status\":{},\"digest\":{},\"message\":{}}}",
        entry.sequence,
        json_string(&entry.operation),
        json_string(&entry.relative_path),
        json_string(&entry.status),
        option_json(entry.digest.as_deref()),
        option_json(entry.message.as_deref())
    )
}

/// 将 staged mutation 目标、步骤、风险和审计编码为 JSON。
fn restore_mutation_plan_json(plan: &RestoreStagedMutationPlan) -> String {
    format!(
        "{{\"plan_id\":{},\"target_root\":{},\"mutation_planned\":{},\"mutation_executed\":{},\"steps\":{},\"affected_paths\":{},\"preview\":{},\"preflight_hash\":{},\"rollback_manifest\":{},\"audit\":{}}}",
        json_string(&plan.plan_id),
        json_string(&plan.target_root),
        plan.mutation_planned,
        plan.mutation_executed,
        json_array(plan.steps.iter().map(restore_mutation_step_json)),
        json_array(plan.affected_paths.iter().map(|path| json_string(path))),
        json_array(plan.preview.iter().map(|entry| json_string(entry))),
        json_string(&plan.preflight_hash),
        json_array(plan.rollback_manifest.iter().map(restore_rollback_entry_json)),
        json_array(plan.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将单个 mutation 步骤及其预期 digest 编码为 JSON。
fn restore_mutation_step_json(step: &RestoreMutationStep) -> String {
    format!(
        "{{\"operation\":{},\"relative_path\":{},\"source_artifact_key\":{},\"expected_digest\":{},\"pre_restore_digest\":{},\"target_kind\":{}}}",
        json_string(step.operation.as_str()),
        json_string(&step.relative_path),
        option_json(step.source_artifact_key.as_deref()),
        option_json(step.expected_digest.as_deref()),
        option_json(step.pre_restore_digest.as_deref()),
        json_string(step.target_kind.as_str())
    )
}

/// 将单个文件回滚条目编码为 JSON。
fn restore_rollback_entry_json(entry: &RestoreRollbackEntry) -> String {
    format!(
        "{{\"relative_path\":{},\"action\":{},\"pre_restore_digest\":{}}}",
        json_string(&entry.relative_path),
        json_string(&entry.action),
        option_json(entry.pre_restore_digest.as_deref())
    )
}

/// 将可选字符串编码为 JSON 字符串或 `null`。
fn option_json(value: Option<&str>) -> String {
    value.map(json_string).unwrap_or_else(|| "null".to_owned())
}

/// 将 restore apply/rollback 锁的 owner、plan 和状态编码为 JSON。
fn restore_apply_lock_json(lock: &eva_backup::RestoreApplyLock) -> String {
    format!(
        "{{\"lock_id\":{},\"plan_id\":{},\"owner\":{},\"status\":{},\"audit\":{}}}",
        json_string(&lock.lock_id),
        json_string(&lock.plan_id),
        json_string(&lock.owner),
        json_string(&lock.status),
        json_array(lock.audit.iter().map(|entry| json_string(entry)))
    )
}
