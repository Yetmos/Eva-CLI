use super::{
    artifact_store_ref, artifact_store_ref_json, backup_cmd, json_array, json_string,
    lock_store_ref, lock_store_ref_json, parse_common_options, required_option, rollback_plan_json,
    snapshot_cmd, success_envelope, trace_for, write_artifact_store_ref, write_command_error,
    write_error_kind, write_lock_store_ref, ArtifactStoreRef, CommonOptions, LockStoreRef,
    OutputFormat, EXIT_OK, EXIT_RUNTIME_UNAVAILABLE,
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
use eva_config::load_project_config;
use eva_core::{EvaError, GenerationId, RequestId};
use eva_lifecycle::{RollbackCoordinator, RollbackPlan};
use eva_observability::TraceFields;
use eva_policy::{HighRiskAction, RuntimePolicyGate, RuntimePolicyRequest};
use eva_storage::FileSystemArtifactStore;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RestoreCommand {
    Plan(RestorePlanOptions),
    Apply(RestoreApplyOptions),
    Rollback(RestoreRollbackOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RestorePlanOptions {
    common: CommonOptions,
    snapshot_id: String,
    request_id: String,
    release_ref: String,
    artifact_store: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RestoreApplyOptions {
    common: CommonOptions,
    plan: Option<PathBuf>,
    confirm: Option<String>,
    artifact_store: Option<PathBuf>,
    lock_store: Option<PathBuf>,
    dry_run: bool,
    owner: String,
    health: RestoreApplyHealthOption,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RestoreRollbackOptions {
    common: CommonOptions,
    plan: Option<PathBuf>,
    confirm: Option<String>,
    artifact_store: Option<PathBuf>,
    lock_store: Option<PathBuf>,
    transaction_log: Option<PathBuf>,
    owner: String,
    health: RestoreApplyHealthOption,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RestorePlanResult {
    backup: BackupCreateResult,
    snapshot: ReleaseSnapshot,
    plan: RestorePlan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RestoreApplyDryRunResult {
    plan: RestoreApplyPlan,
    report: RestoreApplyDryRunReport,
    artifact_store: ArtifactStoreRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RestoreApplyResult {
    report: RestoreApplyReport,
    dry_run: RestoreApplyDryRunReport,
    artifact_store: ArtifactStoreRef,
    lock_store: LockStoreRef,
    mutation_apply: Option<RestoreMutationApplyReport>,
    rollback_plan: Option<RollbackPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RestoreRollbackResult {
    plan: RestoreApplyPlan,
    dry_run: RestoreApplyDryRunReport,
    rollback: RestoreRollbackApplyReport,
    lock: eva_backup::RestoreApplyLock,
    health: RestoreApplyHealthCheck,
    artifact_store: ArtifactStoreRef,
    lock_store: LockStoreRef,
    audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RestoreApplyHealthOption {
    Healthy,
    Failed(String),
}

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
                match create_restore_apply(&options) {
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
            match create_restore_rollback(&options) {
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

fn create_restore_apply(options: &RestoreApplyOptions) -> Result<RestoreApplyResult, EvaError> {
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

    Ok(RestoreApplyResult {
        report,
        dry_run: dry_run.report,
        artifact_store: dry_run.artifact_store,
        lock_store: lock_store_ref(Some(lock_store_path)),
        mutation_apply,
        rollback_plan,
    })
}

fn create_restore_rollback(
    options: &RestoreRollbackOptions,
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

fn resolve_restore_mutation_target_root(project_root: &Path, target_root: &str) -> PathBuf {
    let target = PathBuf::from(target_root);
    if target.is_absolute() {
        target
    } else {
        project_root.join(target)
    }
}

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

fn optional_mutation_field(value: &str) -> Option<String> {
    match value {
        "" | "-" | "none" | "null" => None,
        _ => Some(value.to_owned()),
    }
}

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
            writeln!(writer, "lock: {}", result.lock.lock_id).map_err(write_error_kind)?;
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

fn restore_plan_json(result: &RestorePlanResult) -> String {
    format!(
        "{{\"snapshot\":{},\"backup\":{},\"plan\":{{\"snapshot_id\":{},\"status\":{},\"apply_allowed\":{},\"steps\":{},\"risks\":{},\"audit\":{}}}}}",
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

fn restore_apply_dry_run_json(result: &RestoreApplyDryRunResult) -> String {
    restore_apply_dry_run_report_json(&result.report, &result.artifact_store)
}

fn restore_apply_dry_run_report_json(
    report: &RestoreApplyDryRunReport,
    artifact_store: &ArtifactStoreRef,
) -> String {
    format!(
        "{{\"plan_id\":{},\"status\":{},\"apply_allowed\":{},\"backup_artifact_key\":{},\"expected_digest\":{},\"actual_digest\":{},\"pre_restore_backup_artifact_key\":{},\"pre_restore_expected_digest\":{},\"pre_restore_actual_digest\":{},\"mutation_plan\":{},\"artifact_store\":{},\"audit\":{}}}",
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
        artifact_store_ref_json(artifact_store),
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

fn restore_apply_json(result: &RestoreApplyResult) -> String {
    format!(
        "{{\"plan_id\":{},\"status\":{},\"apply_allowed\":{},\"mutation_executed\":{},\"mutation_plan\":{},\"mutation_apply\":{},\"backup_artifact_key\":{},\"pre_restore_backup_artifact_key\":{},\"lock\":{},\"health\":{{\"healthy\":{},\"message\":{}}},\"artifact_store\":{},\"lock_store\":{},\"dry_run\":{},\"steps\":{},\"risks\":{},\"audit\":{},\"rollback_plan\":{}}}",
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

fn restore_rollback_json(result: &RestoreRollbackResult) -> String {
    format!(
        "{{\"plan_id\":{},\"status\":{},\"rollback_executed\":{},\"rollback\":{},\"mutation_plan\":{},\"lock\":{},\"health\":{{\"healthy\":{},\"message\":{}}},\"artifact_store\":{},\"lock_store\":{},\"dry_run\":{},\"audit\":{}}}",
        json_string(&result.plan.plan_id),
        json_string(&result.rollback.status),
        result.rollback.rollback_executed,
        restore_rollback_apply_json(&result.rollback),
        restore_mutation_plan_json(&result.dry_run.mutation_plan),
        restore_apply_lock_json(&result.lock),
        result.health.healthy,
        json_string(&result.health.message),
        artifact_store_ref_json(&result.artifact_store),
        lock_store_ref_json(&result.lock_store),
        restore_apply_dry_run_report_json(&result.dry_run, &result.artifact_store),
        json_array(result.audit.iter().map(|entry| json_string(entry)))
    )
}

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

fn restore_rollback_entry_json(entry: &RestoreRollbackEntry) -> String {
    format!(
        "{{\"relative_path\":{},\"action\":{},\"pre_restore_digest\":{}}}",
        json_string(&entry.relative_path),
        json_string(&entry.action),
        option_json(entry.pre_restore_digest.as_deref())
    )
}

fn option_json(value: Option<&str>) -> String {
    value.map(json_string).unwrap_or_else(|| "null".to_owned())
}

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
