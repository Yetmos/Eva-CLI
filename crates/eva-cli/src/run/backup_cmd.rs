//! 备份创建子命令：构造计划、选择产物存储并在成功前完成产物验证。

use super::{
    artifact_store_ref, json_array, json_string, option_json, parse_common_options,
    required_option, success_envelope, trace_for, write_artifact_store_ref, write_command_error,
    write_error_kind, ArtifactStoreRef, CommonOptions, OutputFormat, EXIT_OK,
};
use eva_backup::{
    BackupArchiveManifest, BackupEncryptionKey, BackupEncryptionManifest, BackupEntry, BackupPlan,
    BackupResult, BackupScope, BackupService, RemoteBackupTarget,
};
use eva_core::{EvaError, GenerationId, RequestId};
use eva_observability::TraceFields;
use eva_storage::{FileSystemArtifactStore, InMemoryArtifactStore};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Backup 子命令集合。
pub(super) enum BackupCommand {
    /// 创建并验证备份产物。
    Create(
        /// 已解析的备份标识、发布引用、快照角色、存储路径与公共选项。
        BackupCreateOptions,
    ),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 备份创建选项，也是 Snapshot 命令复用的内部契约。
pub(super) struct BackupCreateOptions {
    /// 项目根和输出格式。
    pub(super) common: CommonOptions,
    /// 备份产物的稳定 ID。
    pub(super) artifact_id: String,
    /// 关联审计链路的请求 ID。
    pub(super) request_id: String,
    /// 写入备份 scope 的项目标识。
    pub(super) project_id: String,
    /// 面向审计的备份原因。
    pub(super) reason: String,
    /// 是否只生成计划而不持久化真实内容。
    pub(super) dry_run: bool,
    /// 是否使用本地开发密钥加密归档。
    pub(super) encrypt: bool,
    /// 可选文件系统产物存储；缺省使用内存存储。
    pub(super) artifact_store: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 备份服务结果及实际采用的存储后端描述。
pub(super) struct BackupCreateResult {
    /// 包含计划、manifest 和验证证据的备份结果。
    pub(super) backup: BackupResult,
    /// 用户输出使用的存储后端引用。
    pub(super) artifact_store: ArtifactStoreRef,
}

/// 解析唯一受支持的 `backup create` 子命令。
pub(super) fn parse_backup_command(args: &[String]) -> Result<BackupCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing backup subcommand"))?;
    match subcommand.as_str() {
        "create" => Ok(BackupCommand::Create(parse_backup_create_options(rest)?)),
        value => {
            Err(EvaError::unsupported("unknown backup subcommand")
                .with_context("subcommand", value))
        }
    }
}

/// 执行备份创建并按统一成功或错误契约输出结果。
pub(super) fn execute_backup<W, E>(
    command: BackupCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        BackupCommand::Create(options) => {
            let trace = trace_for("cli.backup.create");
            match create_backup_result(&options) {
                Ok(result) => {
                    write_backup_create(stdout, options.common.output, &result, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "backup.create",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

/// 解析备份 ID、审计字段、存储和加密选项，并预先校验请求 ID。
fn parse_backup_create_options(args: &[String]) -> Result<BackupCreateOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut artifact_id = "backup-v14".to_owned();
    let mut request_id = "req-backup-1".to_owned();
    let mut project_id = "eva-cli".to_owned();
    let mut reason = "pre-upgrade safety checkpoint".to_owned();
    let mut dry_run = false;
    let mut encrypt = false;
    let mut artifact_store = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--artifact" | "--artifact-id" => {
                index += 1;
                artifact_id = required_option(args, index, "artifact option")?.clone();
            }
            "--request-id" | "--task-id" | "--task" => {
                index += 1;
                request_id = required_option(args, index, "request id option")?.clone();
            }
            "--project-id" => {
                index += 1;
                project_id = required_option(args, index, "project id option")?.clone();
            }
            "--reason" => {
                index += 1;
                reason = required_option(args, index, "reason option")?.clone();
            }
            "--artifact-store" | "--artifact-store-dir" => {
                index += 1;
                artifact_store = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "artifact store option",
                )?));
            }
            "--dry-run" => dry_run = true,
            "--encrypt" => encrypt = true,
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    RequestId::parse(&request_id)?;
    Ok(BackupCreateOptions {
        common: parse_common_options(&passthrough)?,
        artifact_id,
        request_id,
        project_id,
        reason,
        dry_run,
        encrypt,
        artifact_store,
    })
}

/// 构造备份计划并交由选定 ArtifactStore 创建和验证。
///
/// 文件系统和内存后端执行同一 `BackupService::create` 路径；返回成功意味着 manifest
/// 与归档验证均已完成。敏感 release pointer 条目仅记录为 redacted 元数据。
pub(super) fn create_backup_result(
    options: &BackupCreateOptions,
) -> Result<BackupCreateResult, EvaError> {
    let scope = BackupScope::new(
        options.project_id.clone(),
        vec![
            BackupEntry::new("config/eva.yaml", "runtime: in_memory_v1.0")?,
            BackupEntry::new("config/adapters/hardware/scale-main.yaml", "enabled: false")?,
            BackupEntry::new("state/release-pointer", options.project_id.as_bytes())?.redacted(),
        ],
    )?;
    let mut plan = BackupPlan::new(
        options.artifact_id.clone(),
        RequestId::parse(&options.request_id)?,
        GenerationId::parse("gen-v14")?,
        "cli",
        options.reason.clone(),
        scope,
    )?;
    if options.dry_run {
        plan = plan.dry_run();
    }
    if options.encrypt {
        plan = plan.encrypted_with(BackupEncryptionKey::local_development());
    }
    let artifact_store = artifact_store_ref(options.artifact_store.as_deref());
    let backup = match &options.artifact_store {
        Some(path) => {
            let mut store = FileSystemArtifactStore::new(path);
            BackupService.create(plan, &mut store)?
        }
        None => {
            let mut store = InMemoryArtifactStore::new();
            BackupService.create(plan, &mut store)?
        }
    };
    Ok(BackupCreateResult {
        backup,
        artifact_store,
    })
}

/// 输出备份 digest、签名、加密和验证状态，以及实际存储后端。
fn write_backup_create<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    result: &BackupCreateResult,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Backup artifact created").map_err(write_error_kind)?;
            writeln!(writer, "artifact: {}", result.backup.manifest.artifact_id)
                .map_err(write_error_kind)?;
            writeln!(writer, "digest: {}", result.backup.manifest.digest)
                .map_err(write_error_kind)?;
            writeln!(
                writer,
                "archive_signature: {}",
                result.backup.manifest.archive.signature.key_id
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "archive_encrypted: {}",
                result.backup.manifest.archive.encrypted
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "verified: {}", result.backup.verification.verified)
                .map_err(write_error_kind)?;
            write_artifact_store_ref(writer, &result.artifact_store)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("backup.create", EXIT_OK, &backup_result_json(result), trace)
        )
        .map_err(write_error_kind),
    }
}

/// 将备份计划、manifest、存储、风险和审计证据编码为稳定 JSON。
pub(super) fn backup_result_json(result: &BackupCreateResult) -> String {
    format!(
        "{{\"artifact_id\":{},\"request_id\":{},\"runtime_generation\":{},\"project_id\":{},\"digest\":{},\"verified\":{},\"artifact_store\":{},\"archive\":{},\"entries\":{},\"risks\":{},\"audit\":{}}}",
        json_string(&result.backup.manifest.artifact_id),
        json_string(result.backup.manifest.request_id.as_str()),
        json_string(result.backup.manifest.runtime_generation.as_str()),
        json_string(&result.backup.manifest.project_id),
        json_string(&result.backup.manifest.digest),
        result.backup.verification.verified,
        artifact_store_ref_json(&result.artifact_store),
        backup_archive_json(&result.backup.manifest.archive),
        json_array(result.backup.manifest.entries.iter().map(|entry| {
            format!(
                "{{\"path\":{},\"size_bytes\":{},\"redacted\":{}}}",
                json_string(&entry.path),
                entry.size_bytes,
                entry.redacted
            )
        })),
        json_array(result.backup.plan.risks.iter().map(|risk| json_string(risk))),
        json_array(result.backup.manifest.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将归档校验和、签名、加密和远端目标元数据编码为 JSON。
fn backup_archive_json(archive: &BackupArchiveManifest) -> String {
    format!(
        "{{\"format\":{},\"artifact_key\":{},\"checksum\":{},\"plaintext_checksum\":{},\"encrypted\":{},\"signature\":{{\"key_id\":{},\"algorithm\":{},\"value\":{}}},\"encryption\":{},\"remote_target\":{}}}",
        json_string(&archive.format),
        json_string(&archive.artifact_key),
        json_string(&archive.checksum),
        json_string(&archive.plaintext_checksum),
        archive.encrypted,
        json_string(&archive.signature.key_id),
        json_string(&archive.signature.algorithm),
        json_string(&archive.signature.value),
        backup_encryption_json(archive.encryption.as_ref()),
        remote_backup_target_json(archive.remote_target.as_ref())
    )
}

/// 将可选加密 manifest 编码为 JSON 或 `null`。
fn backup_encryption_json(encryption: Option<&BackupEncryptionManifest>) -> String {
    encryption
        .map(|encryption| {
            format!(
                "{{\"key_id\":{},\"algorithm\":{},\"plaintext_checksum\":{}}}",
                json_string(&encryption.key_id),
                json_string(&encryption.algorithm),
                json_string(&encryption.plaintext_checksum)
            )
        })
        .unwrap_or_else(|| "null".to_owned())
}

/// 将可选远端备份目标编码为 JSON 或 `null`。
fn remote_backup_target_json(target: Option<&RemoteBackupTarget>) -> String {
    target
        .map(|target| {
            format!(
                "{{\"kind\":{},\"endpoint\":{},\"prefix\":{},\"required\":{}}}",
                json_string(target.kind.as_str()),
                json_string(&target.endpoint),
                json_string(&target.prefix),
                target.required
            )
        })
        .unwrap_or_else(|| "null".to_owned())
}

/// 将实际 ArtifactStore 类型与路径编码为 JSON。
fn artifact_store_ref_json(artifact_store: &ArtifactStoreRef) -> String {
    format!(
        "{{\"kind\":{},\"path\":{}}}",
        json_string(&artifact_store.kind),
        option_json(artifact_store.path.as_deref())
    )
}
