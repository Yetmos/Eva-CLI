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
pub(super) enum BackupCommand {
    Create(BackupCreateOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BackupCreateOptions {
    pub(super) common: CommonOptions,
    pub(super) artifact_id: String,
    pub(super) request_id: String,
    pub(super) project_id: String,
    pub(super) reason: String,
    pub(super) dry_run: bool,
    pub(super) encrypt: bool,
    pub(super) artifact_store: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BackupCreateResult {
    pub(super) backup: BackupResult,
    pub(super) artifact_store: ArtifactStoreRef,
}

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

fn artifact_store_ref_json(artifact_store: &ArtifactStoreRef) -> String {
    format!(
        "{{\"kind\":{},\"path\":{}}}",
        json_string(&artifact_store.kind),
        option_json(artifact_store.path.as_deref())
    )
}
