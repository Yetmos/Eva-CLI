use super::{
    json_array, json_string, lock_store_ref, lock_store_ref_json, option_json,
    parse_common_options, required_option, rollback_plan_json, success_envelope, trace_for,
    write_command_error, write_error_kind, write_lock_store_ref, CommonOptions, LockStoreRef,
    OutputFormat, EXIT_OK,
};
use eva_backup::{MigrationPackageManifest, MigrationPackageService, MigrationPreflight};
use eva_config::load_project_config;
use eva_core::{EvaError, GenerationId};
use eva_lifecycle::{
    DrainCoordinator, DrainPlan, FileSystemSupervisorStateStore, FileSystemUpgradeApplyLockStore,
    GenerationState, InMemorySupervisor, ReleasePointerMutation, RollbackCoordinator, RollbackPlan,
    RuntimeBinaryProbe, RuntimeGeneration, RuntimeHealth, SupervisorHandoffCoordinator,
    SupervisorHandoffReport, SupervisorHandoffRequest, SupervisorReport, UpgradeApplyCoordinator,
    UpgradeApplyLock, UpgradeApplyPlan, UpgradeApplyReport,
};
use eva_observability::TraceFields;
use eva_policy::{HighRiskAction, RuntimePolicyGate, RuntimePolicyRequest};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UpgradeCommand {
    Check(UpgradeCheckOptions),
    Apply(UpgradeApplyOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UpgradeCheckOptions {
    common: CommonOptions,
    from_generation: String,
    to_generation: String,
    from_release: String,
    to_release: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UpgradeApplyOptions {
    common: CommonOptions,
    plan: Option<PathBuf>,
    confirm: Option<String>,
    lock_store: Option<PathBuf>,
    state_store: Option<PathBuf>,
    runtime_binary: Option<PathBuf>,
    owner: String,
    health: UpgradeApplyHealthOption,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UpgradeCheckReport {
    status: String,
    supervisor: SupervisorReport,
    drain: DrainPlan,
    rollback: RollbackPlan,
    migration: MigrationPreflight,
    steps: Vec<String>,
    risks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UpgradeApplyResult {
    report: UpgradeApplyReport,
    lock_store: LockStoreRef,
    state_store: Option<LockStoreRef>,
    handoff: Option<SupervisorHandoffReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UpgradeApplyHealthOption {
    Healthy,
    Failed(String),
}

pub(super) fn parse_upgrade_command(args: &[String]) -> Result<UpgradeCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing upgrade subcommand"))?;
    match subcommand.as_str() {
        "check" => Ok(UpgradeCommand::Check(parse_upgrade_check_options(rest)?)),
        "apply" => Ok(UpgradeCommand::Apply(parse_upgrade_apply_options(rest)?)),
        value => {
            Err(EvaError::unsupported("unknown upgrade subcommand")
                .with_context("subcommand", value))
        }
    }
}

pub(super) fn execute_upgrade<W, E>(
    command: UpgradeCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        UpgradeCommand::Check(options) => {
            let trace = trace_for("cli.upgrade.check");
            match create_upgrade_check(&options) {
                Ok(report) => {
                    write_upgrade_check(stdout, options.common.output, &report, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "upgrade.check",
                    &error,
                    &trace,
                ),
            }
        }
        UpgradeCommand::Apply(options) => {
            let trace = trace_for("cli.upgrade.apply");
            match create_upgrade_apply(&options) {
                Ok(result) => {
                    write_upgrade_apply(stdout, options.common.output, &result, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "upgrade.apply",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

fn parse_upgrade_check_options(args: &[String]) -> Result<UpgradeCheckOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut from_generation = "gen-v13".to_owned();
    let mut to_generation = "gen-v14".to_owned();
    let mut from_release = "1.3.0".to_owned();
    let mut to_release = "1.4.0".to_owned();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--from-generation" => {
                index += 1;
                from_generation = required_option(args, index, "from generation option")?.clone();
            }
            "--to-generation" => {
                index += 1;
                to_generation = required_option(args, index, "to generation option")?.clone();
            }
            "--from-release" => {
                index += 1;
                from_release = required_option(args, index, "from release option")?.clone();
            }
            "--to-release" => {
                index += 1;
                to_release = required_option(args, index, "to release option")?.clone();
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    GenerationId::parse(&from_generation)?;
    GenerationId::parse(&to_generation)?;
    Ok(UpgradeCheckOptions {
        common: parse_common_options(&passthrough)?,
        from_generation,
        to_generation,
        from_release,
        to_release,
    })
}

fn parse_upgrade_apply_options(args: &[String]) -> Result<UpgradeApplyOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut plan = None;
    let mut confirm = None;
    let mut lock_store = None;
    let mut state_store = None;
    let mut runtime_binary = None;
    let mut owner = "cli".to_owned();
    let mut health = UpgradeApplyHealthOption::Healthy;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--plan" => {
                index += 1;
                plan = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "upgrade apply plan option",
                )?));
            }
            "--confirm" => {
                index += 1;
                confirm =
                    Some(required_option(args, index, "upgrade apply confirm option")?.clone());
            }
            "--lock-store" | "--lock-store-dir" => {
                index += 1;
                lock_store = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "upgrade apply lock store option",
                )?));
            }
            "--state-store" | "--state-store-dir" => {
                index += 1;
                state_store = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "upgrade apply state store option",
                )?));
            }
            "--runtime-binary" => {
                index += 1;
                runtime_binary = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "upgrade apply runtime binary option",
                )?));
            }
            "--owner" => {
                index += 1;
                owner = required_option(args, index, "upgrade apply owner option")?.clone();
            }
            "--health" | "--health-check" => {
                index += 1;
                health = parse_upgrade_apply_health(required_option(
                    args,
                    index,
                    "upgrade apply health option",
                )?)?;
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    Ok(UpgradeApplyOptions {
        common: parse_common_options(&passthrough)?,
        plan,
        confirm,
        lock_store,
        state_store,
        runtime_binary,
        owner,
        health,
    })
}

fn parse_upgrade_apply_health(value: &str) -> Result<UpgradeApplyHealthOption, EvaError> {
    match value {
        "healthy" | "pass" | "passed" => Ok(UpgradeApplyHealthOption::Healthy),
        "failed" | "fail" | "unhealthy" => Ok(UpgradeApplyHealthOption::Failed(
            "candidate runtime health check failed".to_owned(),
        )),
        "unavailable" | "missing" => Ok(UpgradeApplyHealthOption::Failed(
            "runtime binary is unavailable".to_owned(),
        )),
        _ => Err(EvaError::invalid_argument(
            "upgrade apply health must be healthy, failed, or unavailable",
        )
        .with_context("health", value)),
    }
}

fn create_upgrade_check(options: &UpgradeCheckOptions) -> Result<UpgradeCheckReport, EvaError> {
    let active = RuntimeGeneration::new(
        GenerationId::parse(&options.from_generation)?,
        options.from_release.clone(),
        GenerationState::Active,
    )?;
    let mut supervisor = InMemorySupervisor::new(active)?;
    let candidate_id = GenerationId::parse(&options.to_generation)?;
    supervisor.start_candidate(candidate_id.clone(), options.to_release.clone())?;
    let supervisor_report = supervisor.report();
    let migration_manifest = MigrationPackageManifest::new(
        "migration-v14",
        options.from_release.clone(),
        options.to_release.clone(),
        vec![
            "backup_manifest".to_owned(),
            "runtime_generation".to_owned(),
        ],
    )?;
    let migration =
        MigrationPackageService.verify_preflight(&migration_manifest, &options.from_release)?;
    let drain = DrainCoordinator.plan(GenerationId::parse(&options.from_generation)?, 0, 30_000)?;
    let rollback = RollbackCoordinator.plan_failed_handoff(
        candidate_id,
        GenerationId::parse(&options.from_generation)?,
        "candidate health or migration preflight failure",
        None,
    )?;
    Ok(UpgradeCheckReport {
        status: if migration.status == "blocked" {
            "blocked".to_owned()
        } else {
            "ready".to_owned()
        },
        supervisor: supervisor_report,
        drain,
        rollback,
        migration,
        steps: vec![
            "create pre-release backup".to_owned(),
            "capture release snapshot".to_owned(),
            "start candidate runtime generation".to_owned(),
            "verify migration package preflight".to_owned(),
            "plan drain and rollback before apply".to_owned(),
        ],
        risks: vec![
            "upgrade check is diagnostic; no runtime process is started by CLI".to_owned(),
            "rollback remains planned until lifecycle apply is explicitly authorized".to_owned(),
        ],
    })
}

fn create_upgrade_apply(options: &UpgradeApplyOptions) -> Result<UpgradeApplyResult, EvaError> {
    let plan_path = options.plan.as_ref().ok_or_else(|| {
        EvaError::invalid_argument("upgrade apply requires --plan")
            .with_context("required_option", "--plan")
    })?;
    let confirm = options.confirm.as_deref().ok_or_else(|| {
        EvaError::invalid_argument("upgrade apply requires --confirm")
            .with_context("required_option", "--confirm")
    })?;
    let lock_store_path = options.lock_store.as_ref().ok_or_else(|| {
        EvaError::invalid_argument("upgrade apply requires --lock-store")
            .with_context("required_option", "--lock-store")
    })?;

    let plan = read_upgrade_apply_plan(plan_path)?;
    if confirm != plan.plan_id {
        return Err(EvaError::permission_denied(
            "upgrade apply confirmation does not match plan id",
        )
        .with_context("confirm", confirm)
        .with_context("plan_id", &plan.plan_id));
    }

    let project = load_project_config(&options.common.project_root)?;
    let policy_gate = RuntimePolicyGate::from_project(&project)?;
    let supervisor_policy =
        policy_gate.decide(RuntimePolicyRequest::new(HighRiskAction::SupervisorHandoff));
    let pointer_policy = policy_gate.decide(RuntimePolicyRequest::new(
        HighRiskAction::ReleasePointerMutation,
    ));
    let mut store = FileSystemUpgradeApplyLockStore::new(lock_store_path);
    let mut report = UpgradeApplyCoordinator.acquire_lock(&mut store, &plan, &options.owner)?;
    report.audit.extend(supervisor_policy.audit.clone());
    report.audit.extend(pointer_policy.audit.clone());
    if !supervisor_policy.allowed {
        report.risks.push(format!(
            "runtime policy denied destructive supervisor handoff: {}",
            supervisor_policy.reason
        ));
        report
            .audit
            .push("supervisor.handoff:policy_denied".to_owned());
    }
    if !pointer_policy.allowed {
        report.risks.push(format!(
            "runtime policy denied release pointer mutation: {}",
            pointer_policy.reason
        ));
        report
            .audit
            .push("release.pointer:policy_denied".to_owned());
    }

    let handoff = if let Some(state_store_path) = options.state_store.as_ref() {
        let mut state_store = FileSystemSupervisorStateStore::new(state_store_path);
        let runtime_binary = options
            .runtime_binary
            .as_ref()
            .map(|path| probe_runtime_binary(path))
            .unwrap_or_else(|| RuntimeBinaryProbe::simulated("runtime-binary:managed-by-cli"));
        let health = match &options.health {
            UpgradeApplyHealthOption::Healthy => RuntimeHealth::healthy(plan.to_generation.clone()),
            UpgradeApplyHealthOption::Failed(message) => RuntimeHealth {
                generation_id: plan.to_generation.clone(),
                healthy: false,
                message: message.clone(),
            },
        };
        let handoff = SupervisorHandoffCoordinator.handoff(
            &mut state_store,
            SupervisorHandoffRequest {
                plan: &plan,
                lock: report.lock.clone(),
                supervisor_policy: &supervisor_policy,
                pointer_policy: &pointer_policy,
                runtime_binary,
                health,
            },
        )?;
        report.status = handoff.status.clone();
        report.apply_allowed = handoff.apply_allowed;
        report.steps = handoff.steps.clone();
        report.risks.extend(handoff.risks.clone());
        report.audit.extend(handoff.audit.clone());
        Some(handoff)
    } else {
        None
    };

    Ok(UpgradeApplyResult {
        report,
        lock_store: lock_store_ref(Some(lock_store_path)),
        state_store: options
            .state_store
            .as_ref()
            .map(|path| lock_store_ref(Some(path))),
        handoff,
    })
}

fn probe_runtime_binary(path: &Path) -> RuntimeBinaryProbe {
    let binary_path = path.display().to_string();
    if !path.exists() {
        return RuntimeBinaryProbe {
            binary_path: binary_path.clone(),
            status: "unavailable".to_owned(),
            audit: vec![
                "runtime.binary:missing".to_owned(),
                format!("runtime.binary:{binary_path}"),
            ],
        };
    }

    let mut child = match ProcessCommand::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return RuntimeBinaryProbe {
                binary_path: binary_path.clone(),
                status: "unavailable".to_owned(),
                audit: vec![
                    "runtime.binary:version_smoke_error".to_owned(),
                    format!("runtime.binary:{binary_path}"),
                    format!("runtime.binary.error:{error}"),
                ],
            };
        }
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                return RuntimeBinaryProbe {
                    binary_path: binary_path.clone(),
                    status: "ready".to_owned(),
                    audit: vec![
                        "runtime.binary:version_smoke".to_owned(),
                        format!("runtime.binary:{binary_path}"),
                        format!(
                            "runtime.binary.exit_code:{}",
                            status
                                .code()
                                .map_or_else(|| "signal".to_owned(), |code| code.to_string())
                        ),
                    ],
                };
            }
            Ok(Some(status)) => {
                return RuntimeBinaryProbe {
                    binary_path: binary_path.clone(),
                    status: "unavailable".to_owned(),
                    audit: vec![
                        "runtime.binary:version_smoke_failed".to_owned(),
                        format!("runtime.binary:{binary_path}"),
                        format!(
                            "runtime.binary.exit_code:{}",
                            status
                                .code()
                                .map_or_else(|| "signal".to_owned(), |code| code.to_string())
                        ),
                    ],
                };
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return RuntimeBinaryProbe {
                    binary_path: binary_path.clone(),
                    status: "unavailable".to_owned(),
                    audit: vec![
                        "runtime.binary:version_smoke_timeout".to_owned(),
                        format!("runtime.binary:{binary_path}"),
                        "runtime.binary.timeout_ms:5000".to_owned(),
                    ],
                };
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                return RuntimeBinaryProbe {
                    binary_path: binary_path.clone(),
                    status: "unavailable".to_owned(),
                    audit: vec![
                        "runtime.binary:version_smoke_error".to_owned(),
                        format!("runtime.binary:{binary_path}"),
                        format!("runtime.binary.error:{error}"),
                    ],
                };
            }
        }
    }
}

fn read_upgrade_apply_plan(path: &Path) -> Result<UpgradeApplyPlan, EvaError> {
    let data = fs::read_to_string(path).map_err(|error| {
        let message = if error.kind() == std::io::ErrorKind::NotFound {
            "upgrade apply plan is missing"
        } else {
            "failed to read upgrade apply plan"
        };
        EvaError::not_found(message)
            .with_context("plan", path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    parse_upgrade_apply_plan(&data)
        .map_err(|error| error.with_context("plan", path.display().to_string()))
}

pub(super) fn parse_upgrade_apply_plan(data: &str) -> Result<UpgradeApplyPlan, EvaError> {
    let mut plan_id = None;
    let mut from_generation = None;
    let mut to_generation = None;
    let mut from_release = None;
    let mut to_release = None;
    for line in data.lines() {
        let line = line.trim_start_matches('\u{feff}');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            return Err(EvaError::invalid_argument(
                "upgrade apply plan line must use key=value format",
            ));
        };
        match key {
            "plan_id" => plan_id = Some(value.to_owned()),
            "from_generation" => from_generation = Some(value.to_owned()),
            "to_generation" => to_generation = Some(value.to_owned()),
            "from_release" => from_release = Some(value.to_owned()),
            "to_release" => to_release = Some(value.to_owned()),
            _ => {
                return Err(EvaError::invalid_argument(
                    "upgrade apply plan contains unsupported field",
                )
                .with_context("field", key));
            }
        }
    }
    UpgradeApplyPlan::new(
        plan_id.ok_or_else(|| EvaError::invalid_argument("upgrade apply plan missing plan_id"))?,
        GenerationId::parse(&from_generation.ok_or_else(|| {
            EvaError::invalid_argument("upgrade apply plan missing from_generation")
        })?)?,
        GenerationId::parse(&to_generation.ok_or_else(|| {
            EvaError::invalid_argument("upgrade apply plan missing to_generation")
        })?)?,
        from_release
            .ok_or_else(|| EvaError::invalid_argument("upgrade apply plan missing from_release"))?,
        to_release
            .ok_or_else(|| EvaError::invalid_argument("upgrade apply plan missing to_release"))?,
    )
}

fn write_upgrade_check<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &UpgradeCheckReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Upgrade check").map_err(write_error_kind)?;
            writeln!(writer, "status: {}", report.status).map_err(write_error_kind)?;
            writeln!(writer, "active: {}", report.supervisor.active_generation)
                .map_err(write_error_kind)?;
            writeln!(writer, "migration: {}", report.migration.status).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("upgrade.check", EXIT_OK, &upgrade_check_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

fn write_upgrade_apply<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    result: &UpgradeApplyResult,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Upgrade apply lock acquired").map_err(write_error_kind)?;
            writeln!(writer, "plan: {}", result.report.plan_id).map_err(write_error_kind)?;
            writeln!(writer, "status: {}", result.report.status).map_err(write_error_kind)?;
            writeln!(writer, "apply_allowed: {}", result.report.apply_allowed)
                .map_err(write_error_kind)?;
            if let Some(handoff) = &result.handoff {
                writeln!(writer, "mutation_executed: {}", handoff.mutation_executed)
                    .map_err(write_error_kind)?;
                writeln!(writer, "active_generation: {}", handoff.active_generation)
                    .map_err(write_error_kind)?;
                if let Some(rollback) = &handoff.rollback_plan {
                    writeln!(writer, "rollback: {}", rollback.status).map_err(write_error_kind)?;
                }
            }
            write_lock_store_ref(writer, &result.lock_store)?;
            if let Some(state_store) = &result.state_store {
                writeln!(writer, "state_store: {}", state_store.kind).map_err(write_error_kind)?;
                if let Some(path) = &state_store.path {
                    writeln!(writer, "state_store_path: {path}").map_err(write_error_kind)?;
                }
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("upgrade.apply", EXIT_OK, &upgrade_apply_json(result), trace)
        )
        .map_err(write_error_kind),
    }
}

fn upgrade_apply_json(result: &UpgradeApplyResult) -> String {
    format!(
        "{{\"plan_id\":{},\"status\":{},\"apply_allowed\":{},\"lock_store\":{},\"state_store\":{},\"lock\":{},\"handoff\":{},\"steps\":{},\"risks\":{},\"audit\":{}}}",
        json_string(&result.report.plan_id),
        json_string(&result.report.status),
        result.report.apply_allowed,
        lock_store_ref_json(&result.lock_store),
        result
            .state_store
            .as_ref()
            .map(lock_store_ref_json)
            .unwrap_or_else(|| "null".to_owned()),
        upgrade_apply_lock_json(&result.report.lock),
        result
            .handoff
            .as_ref()
            .map(supervisor_handoff_json)
            .unwrap_or_else(|| "null".to_owned()),
        json_array(result.report.steps.iter().map(|step| json_string(step))),
        json_array(result.report.risks.iter().map(|risk| json_string(risk))),
        json_array(result.report.audit.iter().map(|entry| json_string(entry)))
    )
}

fn supervisor_handoff_json(report: &SupervisorHandoffReport) -> String {
    format!(
        "{{\"plan_id\":{},\"status\":{},\"apply_allowed\":{},\"mutation_executed\":{},\"runtime_binary\":{},\"active_generation\":{},\"previous_generation\":{},\"release_ref\":{},\"release_pointer\":{},\"rollback_plan\":{},\"steps\":{},\"risks\":{},\"audit\":{}}}",
        json_string(&report.plan_id),
        json_string(&report.status),
        report.apply_allowed,
        report.mutation_executed,
        runtime_binary_probe_json(&report.runtime_binary),
        json_string(&report.active_generation),
        json_string(&report.previous_generation),
        json_string(&report.release_ref),
        report
            .release_pointer
            .as_ref()
            .map(release_pointer_mutation_json)
            .unwrap_or_else(|| "null".to_owned()),
        report
            .rollback_plan
            .as_ref()
            .map(rollback_plan_json)
            .unwrap_or_else(|| "null".to_owned()),
        json_array(report.steps.iter().map(|step| json_string(step))),
        json_array(report.risks.iter().map(|risk| json_string(risk))),
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

fn runtime_binary_probe_json(probe: &RuntimeBinaryProbe) -> String {
    format!(
        "{{\"binary_path\":{},\"status\":{},\"audit\":{}}}",
        json_string(&probe.binary_path),
        json_string(&probe.status),
        json_array(probe.audit.iter().map(|entry| json_string(entry)))
    )
}

fn release_pointer_mutation_json(mutation: &ReleasePointerMutation) -> String {
    format!(
        "{{\"pointer_path\":{},\"previous_generation\":{},\"active_generation\":{},\"release_ref\":{},\"status\":{},\"audit\":{}}}",
        json_string(&mutation.pointer_path),
        json_string(&mutation.previous_generation),
        json_string(&mutation.active_generation),
        json_string(&mutation.release_ref),
        json_string(&mutation.status),
        json_array(mutation.audit.iter().map(|entry| json_string(entry)))
    )
}

fn upgrade_apply_lock_json(lock: &UpgradeApplyLock) -> String {
    format!(
        "{{\"lock_id\":{},\"plan_id\":{},\"owner\":{},\"from_generation\":{},\"to_generation\":{},\"status\":{},\"audit\":{}}}",
        json_string(&lock.lock_id),
        json_string(&lock.plan_id),
        json_string(&lock.owner),
        json_string(lock.from_generation.as_str()),
        json_string(lock.to_generation.as_str()),
        json_string(&lock.status),
        json_array(lock.audit.iter().map(|entry| json_string(entry)))
    )
}

fn upgrade_check_json(report: &UpgradeCheckReport) -> String {
    format!(
        "{{\"status\":{},\"supervisor\":{},\"drain\":{},\"rollback\":{},\"migration\":{},\"steps\":{},\"risks\":{}}}",
        json_string(&report.status),
        supervisor_report_json(&report.supervisor),
        drain_plan_json(&report.drain),
        rollback_plan_json(&report.rollback),
        migration_preflight_json(&report.migration),
        json_array(report.steps.iter().map(|step| json_string(step))),
        json_array(report.risks.iter().map(|risk| json_string(risk)))
    )
}

fn supervisor_report_json(report: &SupervisorReport) -> String {
    format!(
        "{{\"active_generation\":{},\"candidate_generation\":{},\"healthy\":{},\"audit\":{}}}",
        json_string(report.active_generation.as_str()),
        option_json(report.candidate_generation.as_ref().map(|id| id.as_str())),
        report.healthy,
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

fn drain_plan_json(plan: &DrainPlan) -> String {
    format!(
        "{{\"generation_id\":{},\"inflight_tasks\":{},\"timeout_ms\":{},\"accepts_new_work\":{},\"status\":{},\"audit\":{}}}",
        json_string(plan.generation_id.as_str()),
        plan.inflight_tasks,
        plan.timeout_ms,
        plan.accepts_new_work,
        json_string(plan.status.as_str()),
        json_array(plan.audit.iter().map(|entry| json_string(entry)))
    )
}

fn migration_preflight_json(report: &MigrationPreflight) -> String {
    format!(
        "{{\"package_id\":{},\"status\":{},\"warnings\":{},\"audit\":{}}}",
        json_string(&report.package_id),
        json_string(&report.status),
        json_array(report.warnings.iter().map(|warning| json_string(warning))),
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}
