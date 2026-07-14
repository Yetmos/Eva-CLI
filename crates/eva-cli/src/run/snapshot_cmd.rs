//! 发布快照创建与晋升规划子命令；晋升只生成 pointer 计划，不直接移动发布指针。

use super::{
    backup_cmd, json_array, json_string, parse_common_options, required_option, success_envelope,
    trace_for, write_artifact_store_ref, write_command_error, write_error_kind, CommonOptions,
    OutputFormat, EXIT_OK,
};
use backup_cmd::{BackupCreateOptions, BackupCreateResult};
use eva_backup::{ReleasePointerPlan, ReleaseSnapshot, ReleaseSnapshotService, SnapshotRole};
use eva_core::{EvaError, RequestId};
use eva_observability::TraceFields;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Snapshot 子命令及其已解析选项。
pub(super) enum SnapshotCommand {
    /// 从已验证备份创建发布快照。
    Create(
        /// 已解析的快照、备份、发布引用、角色、存储与公共选项。
        SnapshotCreateOptions,
    ),
    /// 在显式确认后生成快照晋升与 pointer 计划。
    Promote(
        /// 已解析的快照、存储、确认标志与公共选项。
        SnapshotPromoteOptions,
    ),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 快照创建选项。
pub(super) struct SnapshotCreateOptions {
    /// 项目根和输出格式。
    pub(super) common: CommonOptions,
    /// 发布快照 ID。
    pub(super) snapshot_id: String,
    /// 关联备份和审计的请求 ID。
    pub(super) request_id: String,
    /// 快照对应的发布引用。
    pub(super) release_ref: String,
    /// 快照处于发布前或发布后阶段。
    pub(super) role: SnapshotRole,
    /// 可选文件系统产物存储。
    pub(super) artifact_store: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 快照晋升选项，包含防误操作确认值。
pub(super) struct SnapshotPromoteOptions {
    /// 项目根和输出格式。
    common: CommonOptions,
    /// 要晋升的快照 ID。
    snapshot_id: String,
    /// 必须与快照 ID 匹配的显式确认值。
    confirm: Option<String>,
    /// 审计请求 ID。
    request_id: String,
    /// 目标发布引用。
    release_ref: String,
    /// 备份与快照使用的可选产物存储。
    artifact_store: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 晋升规划的组合结果；保留前置备份、快照和 release pointer 计划。
struct SnapshotPromoteResult {
    /// 晋升前创建并验证的备份。
    backup: BackupCreateResult,
    /// 基于备份生成的发布后快照。
    snapshot: ReleaseSnapshot,
    /// 未执行的 release pointer 变更计划。
    pointer_plan: ReleasePointerPlan,
}

/// 解析 `snapshot create|promote` 子命令。
pub(super) fn parse_snapshot_command(args: &[String]) -> Result<SnapshotCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing snapshot subcommand"))?;
    match subcommand.as_str() {
        "create" => Ok(SnapshotCommand::Create(parse_snapshot_create_options(
            rest,
        )?)),
        "promote" => Ok(SnapshotCommand::Promote(parse_snapshot_promote_options(
            rest,
        )?)),
        value => {
            Err(EvaError::unsupported("unknown snapshot subcommand")
                .with_context("subcommand", value))
        }
    }
}

/// 执行快照创建或晋升规划，并使用统一输出与错误映射。
pub(super) fn execute_snapshot<W, E>(
    command: SnapshotCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        SnapshotCommand::Create(options) => {
            let trace = trace_for("cli.snapshot.create");
            match create_snapshot_result(&options) {
                Ok((backup, snapshot)) => {
                    write_snapshot_create(
                        stdout,
                        options.common.output,
                        &backup,
                        &snapshot,
                        &trace,
                    )?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "snapshot.create",
                    &error,
                    &trace,
                ),
            }
        }
        SnapshotCommand::Promote(options) => {
            let trace = trace_for("cli.snapshot.promote");
            match create_snapshot_promote(&options) {
                Ok(result) => {
                    write_snapshot_promote(stdout, options.common.output, &result, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "snapshot.promote",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

/// 解析快照创建选项并校验请求 ID 与角色。
fn parse_snapshot_create_options(args: &[String]) -> Result<SnapshotCreateOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut snapshot_id = "snapshot-v14".to_owned();
    let mut request_id = "req-snapshot-1".to_owned();
    let mut release_ref = "1.4.0".to_owned();
    let mut role = SnapshotRole::PreRelease;
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
            "--role" => {
                index += 1;
                role = parse_snapshot_role(required_option(args, index, "role option")?)?;
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
    Ok(SnapshotCreateOptions {
        common: parse_common_options(&passthrough)?,
        snapshot_id,
        request_id,
        release_ref,
        role,
        artifact_store,
    })
}

/// 解析晋升选项；确认值在执行阶段与快照 ID 做精确比较。
fn parse_snapshot_promote_options(args: &[String]) -> Result<SnapshotPromoteOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut snapshot_id = "snapshot-v14".to_owned();
    let mut confirm = None;
    let mut request_id = "req-snapshot-promote-1".to_owned();
    let mut release_ref = "1.4.0".to_owned();
    let mut artifact_store = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--snapshot-id" | "--snapshot" => {
                index += 1;
                snapshot_id = required_option(args, index, "snapshot option")?.clone();
            }
            "--confirm" => {
                index += 1;
                confirm =
                    Some(required_option(args, index, "snapshot promote confirm option")?.clone());
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
    Ok(SnapshotPromoteOptions {
        common: parse_common_options(&passthrough)?,
        snapshot_id,
        confirm,
        request_id,
        release_ref,
        artifact_store,
    })
}

/// 先创建已验证备份，再据其 manifest 创建发布快照。
/// 前置备份失败时不会产生快照，避免快照引用不可恢复的状态。
pub(super) fn create_snapshot_result(
    options: &SnapshotCreateOptions,
) -> Result<(BackupCreateResult, ReleaseSnapshot), EvaError> {
    let backup_options = BackupCreateOptions {
        common: options.common.clone(),
        artifact_id: format!("backup-for-{}", options.snapshot_id),
        request_id: options.request_id.clone(),
        project_id: "eva-cli".to_owned(),
        reason: "snapshot capture requires verified backup artifact".to_owned(),
        dry_run: false,
        encrypt: false,
        artifact_store: options.artifact_store.clone(),
    };
    let backup = backup_cmd::create_backup_result(&backup_options)?;
    let snapshot = ReleaseSnapshotService.create(
        options.snapshot_id.clone(),
        options.role,
        options.release_ref.clone(),
        RequestId::parse(&options.request_id)?,
        &backup.backup.manifest,
        "healthy",
    )?;
    Ok((backup, snapshot))
}

/// 校验显式确认后创建备份、发布后快照和 pointer 计划。
///
/// 确认缺失或不匹配会在任何晋升计划产生前失败；返回的 pointer plan 仍是 plan-first
/// 结果，CLI 不在此路径写 release pointer。
fn create_snapshot_promote(
    options: &SnapshotPromoteOptions,
) -> Result<SnapshotPromoteResult, EvaError> {
    let confirm = options.confirm.as_deref().ok_or_else(|| {
        EvaError::invalid_argument("snapshot promote requires --confirm")
            .with_context("required_option", "--confirm")
    })?;
    let backup = backup_cmd::create_backup_result(&BackupCreateOptions {
        common: options.common.clone(),
        artifact_id: format!("backup-for-{}", options.snapshot_id),
        request_id: options.request_id.clone(),
        project_id: "eva-cli".to_owned(),
        reason: format!("snapshot promote {}", options.snapshot_id),
        dry_run: false,
        encrypt: false,
        artifact_store: options.artifact_store.clone(),
    })?;
    let snapshot = ReleaseSnapshotService.create(
        options.snapshot_id.clone(),
        SnapshotRole::PostRelease,
        options.release_ref.clone(),
        RequestId::parse(&options.request_id)?,
        &backup.backup.manifest,
        "healthy",
    )?;
    let pointer_plan = ReleaseSnapshotService.release_pointer_plan(&snapshot, confirm)?;
    Ok(SnapshotPromoteResult {
        backup,
        snapshot,
        pointer_plan,
    })
}

/// 输出快照创建结果和前置备份存储信息。
fn write_snapshot_create<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    backup: &BackupCreateResult,
    snapshot: &ReleaseSnapshot,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Release snapshot created").map_err(write_error_kind)?;
            writeln!(writer, "snapshot: {}", snapshot.snapshot_id).map_err(write_error_kind)?;
            writeln!(writer, "backup: {}", backup.backup.manifest.artifact_id)
                .map_err(write_error_kind)?;
            writeln!(writer, "role: {}", snapshot.role.as_str()).map_err(write_error_kind)?;
            write_artifact_store_ref(writer, &backup.artifact_store)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "snapshot.create",
                EXIT_OK,
                &snapshot_create_json(backup, snapshot),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

/// 输出晋升组合结果，明确展示 pointer plan 而非已执行变更。
fn write_snapshot_promote<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    result: &SnapshotPromoteResult,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Snapshot promote plan").map_err(write_error_kind)?;
            writeln!(writer, "snapshot: {}", result.snapshot.snapshot_id)
                .map_err(write_error_kind)?;
            writeln!(writer, "status: {}", result.pointer_plan.status).map_err(write_error_kind)?;
            writeln!(
                writer,
                "apply_allowed: {}",
                result.pointer_plan.apply_allowed
            )
            .map_err(write_error_kind)?;
            write_artifact_store_ref(writer, &result.backup.artifact_store)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "snapshot.promote",
                EXIT_OK,
                &snapshot_promote_json(result),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

/// 将发布快照及其验证、风险和审计字段编码为 JSON。
pub(super) fn snapshot_json(snapshot: &ReleaseSnapshot) -> String {
    format!(
        "{{\"snapshot_id\":{},\"role\":{},\"release_ref\":{},\"request_id\":{},\"runtime_generation\":{},\"backup_artifact_id\":{},\"backup_digest\":{},\"health_status\":{},\"audit\":{}}}",
        json_string(&snapshot.snapshot_id),
        json_string(snapshot.role.as_str()),
        json_string(&snapshot.release_ref),
        json_string(snapshot.request_id.as_str()),
        json_string(snapshot.runtime_generation.as_str()),
        json_string(&snapshot.backup_artifact_id),
        json_string(&snapshot.backup_digest),
        json_string(&snapshot.health_status),
        json_array(snapshot.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将快照与前置备份组合为创建结果 JSON。
fn snapshot_create_json(backup: &BackupCreateResult, snapshot: &ReleaseSnapshot) -> String {
    format!(
        "{{\"snapshot\":{},\"backup\":{}}}",
        snapshot_json(snapshot),
        backup_cmd::backup_result_json(backup)
    )
}

/// 将晋升备份、快照和 pointer 计划组合为 JSON。
fn snapshot_promote_json(result: &SnapshotPromoteResult) -> String {
    format!(
        "{{\"snapshot\":{},\"backup\":{},\"release_pointer_plan\":{}}}",
        snapshot_json(&result.snapshot),
        backup_cmd::backup_result_json(&result.backup),
        release_pointer_plan_json(&result.pointer_plan)
    )
}

/// 将 release pointer 计划编码为 JSON，保留 `apply_allowed` 和全部风险。
fn release_pointer_plan_json(plan: &ReleasePointerPlan) -> String {
    format!(
        "{{\"snapshot_id\":{},\"release_ref\":{},\"runtime_generation\":{},\"pointer_path\":{},\"status\":{},\"apply_allowed\":{},\"steps\":{},\"risks\":{},\"audit\":{}}}",
        json_string(&plan.snapshot_id),
        json_string(&plan.release_ref),
        json_string(plan.runtime_generation.as_str()),
        json_string(&plan.pointer_path),
        json_string(&plan.status),
        plan.apply_allowed,
        json_array(plan.steps.iter().map(|step| json_string(step))),
        json_array(plan.risks.iter().map(|risk| json_string(risk))),
        json_array(plan.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 解析快照阶段的兼容别名，未知值保留在错误上下文中。
fn parse_snapshot_role(value: &str) -> Result<SnapshotRole, EvaError> {
    match value {
        "pre_release" | "pre-release" | "pre" => Ok(SnapshotRole::PreRelease),
        "post_release" | "post-release" | "post" => Ok(SnapshotRole::PostRelease),
        other => {
            Err(EvaError::invalid_argument("unknown snapshot role").with_context("role", other))
        }
    }
}
