//! 在隔离工作目录中执行经过清单和输入模式校验的工作流技能。
//!
//! 运行器命令必须命中白名单且不经 shell；路径段会规范化并限制在工作根与制品根内，防止
//! 技能通过相对路径逃逸。stdout、stderr 与声明制品统一执行大小限制和凭据脱敏，只有实际
//! 持久化成功的制品才会进入证据报告。
//! Workflow skill Adapter transport runner.

use crate::credential_vault::minimal_process_env;
use crate::credential_vault::{
    default_credential_vault, sanitize_error_with_values, CredentialSessionLease, CredentialVault,
};
use crate::manifest::{AdapterHandle, SkillInputSchema};
use crate::process_backend::{OsProcessBackend, ProviderProcessHandle, ProviderProcessSpawner};
use crate::runtime::{AdapterInvocation, AdapterInvokeReport};
use crate::stream::{
    capture_provider_bytes, collect_provider_stream, provider_stream_audit, provider_stream_key,
    provider_stream_summary_json, redact_provider_stream_bytes, ProviderStreamArtifact,
    ProviderStreamCapture, ProviderStreamConfig, DEFAULT_STREAM_CHUNK_SIZE_BYTES,
    DEFAULT_STREAM_PREVIEW_LIMIT_BYTES,
};
use crate::supervisor::validate_credential_scope_for_provider;
use eva_config::ProviderRunAsIdentity;
use eva_core::{AdapterId, EvaError, RequestId};
use eva_storage::{ArtifactRecord, FileSystemArtifactStore};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt;
use std::fs;
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "controlled workflow skill execution with schema gates and artifact evidence";

/// 定义技能进程、隔离工作目录、输出和制品存储的执行边界。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRunnerConfig {
    /// 记录 `allowed_commands` 字段对应的值。
    pub allowed_commands: BTreeSet<String>,
    /// 记录 `timeout_ms` 字段对应的值。
    pub timeout_ms: u64,
    /// 记录 `output_limit_bytes` 字段对应的值。
    pub output_limit_bytes: usize,
    /// 记录 `preview_limit_bytes` 字段对应的值。
    pub preview_limit_bytes: usize,
    /// 记录 `stream_chunk_size_bytes` 字段对应的值。
    pub stream_chunk_size_bytes: usize,
    /// 记录 `artifact_root` 字段对应的值。
    pub artifact_root: PathBuf,
    /// 记录 `work_root` 字段对应的值。
    pub work_root: PathBuf,
}

/// 表示 `SkillRunnerInvocation` 数据结构。
#[derive(Clone, PartialEq, Eq)]
pub struct SkillRunnerInvocation {
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `request_id` 字段对应的值。
    pub request_id: RequestId,
    /// 记录 `skill_id` 字段对应的值。
    pub skill_id: String,
    /// 记录 `entry_type` 字段对应的值。
    pub entry_type: String,
    /// 记录 `command` 字段对应的值。
    pub command: Option<String>,
    /// 记录 `args` 字段对应的值。
    pub args: Vec<String>,
    /// 记录 `env` 字段对应的值。
    pub env: BTreeMap<String, String>,
    /// 记录 `input` 字段对应的值。
    pub input: String,
}

impl fmt::Debug for SkillRunnerInvocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SkillRunnerInvocation")
            .field("adapter_id", &self.adapter_id)
            .field("request_id", &self.request_id)
            .field("skill_id", &self.skill_id)
            .field("entry_type", &self.entry_type)
            .field("command_present", &self.command.is_some())
            .field("args_count", &self.args.len())
            .field("env_names", &self.env.keys().collect::<Vec<_>>())
            .field("input_len", &self.input.len())
            .finish()
    }
}

/// 表示 `SkillRunReport` 数据结构。
#[derive(Clone, PartialEq, Eq)]
pub struct SkillRunReport {
    /// 记录 `runner` 字段对应的值。
    pub runner: String,
    /// 记录 `status` 字段对应的值。
    pub status: SkillRunStatus,
    /// 记录 `exit_code` 字段对应的值。
    pub exit_code: Option<i32>,
    /// 记录 `stdout` 字段对应的值。
    pub stdout: Vec<u8>,
    /// 记录 `stderr` 字段对应的值。
    pub stderr: Vec<u8>,
    /// 记录 `stdout_stream` 字段对应的值。
    pub stdout_stream: ProviderStreamCapture,
    /// 记录 `stderr_stream` 字段对应的值。
    pub stderr_stream: ProviderStreamCapture,
    /// 记录 `duration_ms` 字段对应的值。
    pub duration_ms: u128,
    /// 记录 `working_dir` 字段对应的值。
    pub working_dir: PathBuf,
    /// 记录 `artifact_root` 字段对应的值。
    pub artifact_root: PathBuf,
    /// 记录 `artifacts` 字段对应的值。
    pub artifacts: Vec<SkillArtifactEvidence>,
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

impl fmt::Debug for SkillRunReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SkillRunReport")
            .field("runner", &self.runner)
            .field("status", &self.status)
            .field("exit_code", &self.exit_code)
            .field("stdout_len", &self.stdout.len())
            .field("stderr_len", &self.stderr.len())
            .field("stdout_stream", &"[REDACTED_STREAM]")
            .field("stderr_stream", &"[REDACTED_STREAM]")
            .field("duration_ms", &self.duration_ms)
            .field("working_dir", &self.working_dir)
            .field("artifact_root", &self.artifact_root)
            .field("artifacts", &self.artifacts)
            .field("audit_count", &self.audit.len())
            .finish()
    }
}

/// 表示 `SkillArtifactEvidence` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillArtifactEvidence {
    /// 记录 `key` 字段对应的值。
    pub key: String,
    /// 记录 `digest` 字段对应的值。
    pub digest: String,
    /// 记录 `size_bytes` 字段对应的值。
    pub size_bytes: usize,
    /// 记录 `content_type` 字段对应的值。
    pub content_type: String,
}

/// 定义 `SkillRunStatus` 可取的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillRunStatus {
    /// 表示 `Completed` 枚举分支。
    Completed,
    /// 表示 `Failed` 枚举分支。
    Failed,
    /// 表示 `Timeout` 枚举分支。
    Timeout,
    /// 表示 `OutputLimitExceeded` 枚举分支。
    OutputLimitExceeded,
}

/// 无状态技能运行器；每次运行创建独立工作目录与证据集合。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SkillRunner;

/// 表示 `RunPaths` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunPaths {
    /// 记录 `working_dir` 字段对应的值。
    working_dir: PathBuf,
    /// 记录 `artifact_dir` 字段对应的值。
    artifact_dir: PathBuf,
    /// 记录 `artifact_root` 字段对应的值。
    artifact_root: PathBuf,
}

/// 表示 `RawSkillRunReport` 数据结构。
#[derive(Clone, PartialEq, Eq)]
struct RawSkillRunReport {
    /// 记录 `runner` 字段对应的值。
    runner: String,
    /// 记录 `status` 字段对应的值。
    status: SkillRunStatus,
    /// 记录 `exit_code` 字段对应的值。
    exit_code: Option<i32>,
    /// 记录 `stdout` 字段对应的值。
    stdout: Vec<u8>,
    /// 记录 `stderr` 字段对应的值。
    stderr: Vec<u8>,
    /// 记录 `stdout_stream` 字段对应的值。
    stdout_stream: ProviderStreamCapture,
    /// 记录 `stderr_stream` 字段对应的值。
    stderr_stream: ProviderStreamCapture,
    /// 记录 `duration_ms` 字段对应的值。
    duration_ms: u128,
    /// 记录 `audit` 字段对应的值。
    audit: Vec<String>,
}

impl fmt::Debug for RawSkillRunReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawSkillRunReport")
            .field("runner", &self.runner)
            .field("status", &self.status)
            .field("exit_code", &self.exit_code)
            .field("stdout_len", &self.stdout.len())
            .field("stderr_len", &self.stderr.len())
            .field("stdout_stream", &"[REDACTED_STREAM]")
            .field("stderr_stream", &"[REDACTED_STREAM]")
            .field("duration_ms", &self.duration_ms)
            .field("audit_count", &self.audit.len())
            .finish()
    }
}

enum SkillChild {
    Supervised(ProviderProcessHandle),
}

impl SkillChild {
    fn pid(&self) -> u32 {
        match self {
            Self::Supervised(handle) => handle.pid(),
        }
    }

    fn take_stdin(&mut self) -> Option<Box<dyn Write + Send>> {
        match self {
            Self::Supervised(handle) => handle
                .take_stdin()
                .map(|stdin| Box::new(stdin) as Box<dyn Write + Send>),
        }
    }

    fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>> {
        match self {
            Self::Supervised(handle) => handle
                .take_stdout()
                .map(|stdout| Box::new(stdout) as Box<dyn Read + Send>),
        }
    }

    fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>> {
        match self {
            Self::Supervised(handle) => handle
                .take_stderr()
                .map(|stderr| Box::new(stderr) as Box<dyn Read + Send>),
        }
    }

    fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>, EvaError> {
        match self {
            Self::Supervised(handle) => handle.try_wait(),
        }
    }
}

fn terminate_skill_child(child: &mut SkillChild) {
    match child {
        SkillChild::Supervised(handle) => {
            let _ = handle.force_terminate();
        }
    }
}

/// 定义 `ParsedJsonValue` 可取的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedJsonValue {
    /// 表示 `String` 枚举分支。
    String(
        /// 保存已完成 JSON 转义解析的字符串内容。
        String,
    ),
    /// 表示 `Other` 枚举分支。
    Other,
}

impl SkillRunnerConfig {
    /// 创建并初始化当前类型的实例。
    pub fn new(
        allowed_commands: impl IntoIterator<Item = impl Into<String>>,
        timeout_ms: u64,
        output_limit_bytes: usize,
        artifact_root: impl Into<PathBuf>,
        work_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            allowed_commands: allowed_commands
                .into_iter()
                .map(Into::into)
                .collect::<BTreeSet<_>>(),
            timeout_ms,
            output_limit_bytes,
            preview_limit_bytes: DEFAULT_STREAM_PREVIEW_LIMIT_BYTES,
            stream_chunk_size_bytes: DEFAULT_STREAM_CHUNK_SIZE_BYTES,
            artifact_root: artifact_root.into(),
            work_root: work_root.into(),
        }
    }
}

impl SkillRunStatus {
    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::OutputLimitExceeded => "output_limit_exceeded",
        }
    }
}

impl SkillRunner {
    /// 校验配置并准备工作目录，执行选定运行器后统一持久化输出与声明制品。
    pub fn run(
        &self,
        config: &SkillRunnerConfig,
        invocation: SkillRunnerInvocation,
    ) -> Result<SkillRunReport, EvaError> {
        self.run_with_spawner_as(config, invocation, None, &ProviderRunAsIdentity::Current)
    }

    /// Runs a process-backed skill through an optional central process owner.
    pub fn run_with_spawner(
        &self,
        config: &SkillRunnerConfig,
        invocation: SkillRunnerInvocation,
        process_spawner: Option<&dyn ProviderProcessSpawner>,
    ) -> Result<SkillRunReport, EvaError> {
        self.run_with_spawner_as(
            config,
            invocation,
            process_spawner,
            &ProviderRunAsIdentity::Current,
        )
    }

    /// Runs a skill with the manifest-selected provider identity. Built-in
    /// skills are process-free and therefore reject non-current identities.
    pub fn run_with_spawner_as(
        &self,
        config: &SkillRunnerConfig,
        invocation: SkillRunnerInvocation,
        process_spawner: Option<&dyn ProviderProcessSpawner>,
        run_as: &ProviderRunAsIdentity,
    ) -> Result<SkillRunReport, EvaError> {
        let backend = OsProcessBackend::new();
        let process_spawner = process_spawner.unwrap_or(&backend);
        if invocation.command.is_some() {
            if !matches!(run_as, ProviderRunAsIdentity::Current) {
                return Err(EvaError::unsupported(
                    "process-backed Skill run-as requires a controlled workdir handoff",
                )
                .with_context("run_as_kind", run_as.kind())
                .with_context("skill_id", &invocation.skill_id));
            }
            process_spawner.validate_provider_run_as(run_as)?;
        } else if !matches!(run_as, ProviderRunAsIdentity::Current) {
            return Err(EvaError::permission_denied(
                "process-free Skill entry cannot apply a run-as identity",
            )
            .with_context("run_as_kind", run_as.kind())
            .with_context("skill_id", &invocation.skill_id));
        }
        validate_runner_config(config, &invocation)?;
        let sensitive_values = sensitive_values(invocation.env.values());
        let paths = prepare_run_paths(config, &invocation, &sensitive_values)?;
        let raw = if let Some(command) = &invocation.command {
            run_process(
                config,
                &paths,
                &invocation,
                command,
                &sensitive_values,
                process_spawner,
                run_as,
            )?
        } else if invocation.entry_type == "codex_skill" {
            run_builtin_codex_skill(&paths, &invocation, &sensitive_values)?
        } else {
            return Err(EvaError::unsupported(
                "skill entry type requires an explicit runner command",
            )
            .with_context("entry_type", invocation.entry_type)
            .with_context("skill_id", invocation.skill_id));
        };
        let artifacts = persist_run_artifacts(&paths, &invocation, &raw, &sensitive_values)?;
        let mut stdout_stream = raw.stdout_stream;
        let mut stderr_stream = raw.stderr_stream;
        if stdout_stream.artifact.is_none() {
            stdout_stream.artifact = artifacts
                .iter()
                .find(|artifact| artifact.key.ends_with("/stdout"))
                .map(provider_stream_artifact_from_skill_evidence);
        }
        if stderr_stream.artifact.is_none() {
            stderr_stream.artifact = artifacts
                .iter()
                .find(|artifact| artifact.key.ends_with("/stderr"))
                .map(provider_stream_artifact_from_skill_evidence);
        }

        Ok(SkillRunReport {
            runner: raw.runner,
            status: raw.status,
            exit_code: raw.exit_code,
            stdout: raw.stdout,
            stderr: raw.stderr,
            stdout_stream,
            stderr_stream,
            duration_ms: raw.duration_ms,
            working_dir: paths.working_dir,
            artifact_root: paths.artifact_root,
            artifacts,
            audit: raw.audit,
        })
    }
}

/// 校验技能种类、运行时门禁和输入模式，再绑定本次请求的凭据与隔离路径。
pub fn invoke(
    handle: &AdapterHandle,
    invocation: AdapterInvocation,
) -> Result<AdapterInvokeReport, EvaError> {
    let vault = default_credential_vault();
    invoke_with_spawner_and_vault(handle, invocation, None, vault.as_ref())
}

/// Invokes a skill while optionally attaching the central process registrar.
pub fn invoke_with_spawner(
    handle: &AdapterHandle,
    invocation: AdapterInvocation,
    process_spawner: Option<&dyn ProviderProcessSpawner>,
) -> Result<AdapterInvokeReport, EvaError> {
    let vault = default_credential_vault();
    invoke_with_spawner_and_vault(handle, invocation, process_spawner, vault.as_ref())
}

/// Invoke a Skill with an explicit credential authority.
pub fn invoke_with_spawner_and_vault(
    handle: &AdapterHandle,
    invocation: AdapterInvocation,
    process_spawner: Option<&dyn ProviderProcessSpawner>,
    vault: &dyn CredentialVault,
) -> Result<AdapterInvokeReport, EvaError> {
    let skill = handle.skill_name().ok_or_else(|| {
        EvaError::invalid_argument("Skill adapter is missing skill.id")
            .with_context("adapter_id", handle.id.as_str())
    })?;
    if handle.skill_kind.as_deref() != Some("workflow_skill") {
        return Err(
            EvaError::permission_denied("Skill adapter kind is not invokable")
                .with_context("adapter_id", handle.id.as_str())
                .with_context("skill_kind", handle.skill_kind.as_deref().unwrap_or("")),
        );
    }
    if handle.skill_runtime_gate.as_deref() != Some("normal") {
        return Err(
            EvaError::permission_denied("Skill runtime gate is not allowed")
                .with_context("adapter_id", handle.id.as_str())
                .with_context(
                    "runtime_gate",
                    handle.skill_runtime_gate.as_deref().unwrap_or(""),
                ),
        );
    }
    validate_input_size(handle, &invocation.input)?;
    validate_skill_input(handle, &invocation.input)?;

    let entry_type = handle
        .skill_entry_type
        .clone()
        .unwrap_or_else(|| "codex_skill".to_owned());
    let command = handle
        .skill_runner_command
        .clone()
        .or_else(|| handle.command.clone());
    let args = if handle.skill_runner_command.is_some() {
        handle.skill_runner_args.clone()
    } else {
        handle.args.clone()
    };
    let has_credential_header = handle
        .headers
        .values()
        .any(|value| value.strip_prefix("env:").is_some());
    if has_credential_header {
        return Err(
            EvaError::unsupported("Skill transport does not support credential headers")
                .with_context("skill_id", skill),
        );
    }

    // Keep identity admission ahead of credential reads and workdir
    // creation for direct transport callers as well as the runtime path.
    if command.is_some() {
        if !matches!(&handle.provider.run_as, ProviderRunAsIdentity::Current) {
            return Err(EvaError::unsupported(
                "process-backed Skill run-as requires a controlled workdir handoff",
            )
            .with_context("run_as_kind", handle.provider.run_as.kind())
            .with_context("skill_id", skill));
        }
        match process_spawner {
            Some(spawner) => spawner.validate_provider_run_as(&handle.provider.run_as)?,
            None => OsProcessBackend::new().validate_run_as(&handle.provider.run_as)?,
        }
    } else if !matches!(&handle.provider.run_as, ProviderRunAsIdentity::Current) {
        return Err(EvaError::permission_denied(
            "process-free Skill cannot apply a run-as identity",
        )
        .with_context("run_as_kind", handle.provider.run_as.kind())
        .with_context("skill_id", skill));
    }

    if command.is_none()
        && (!handle.credential_env.is_empty() || !handle.provider.vault_secrets.is_empty())
    {
        return Err(EvaError::unsupported(
            "process-free Skill cannot consume provider credentials",
        )
        .with_context("skill_id", skill));
    }

    let credential_scope = validate_credential_scope_for_provider(
        invocation.credential_scope(),
        &handle.id,
        &invocation.request_id,
        &invocation.capability,
        !handle.credential_env.is_empty() || !handle.provider.vault_secrets.is_empty(),
    )?
    .cloned();

    let trace = invocation.trace_for_adapter(&handle.id);
    let request_id = invocation.request_id;
    let capability = invocation.capability;
    let mut credential_env = CredentialSessionLease::open(
        vault,
        credential_scope.as_ref(),
        &handle.provider.vault_secrets,
        &handle.credential_env,
    )?;
    let mut child_env = BTreeMap::new();
    credential_env.inject_env(&mut child_env);
    if let Some(scope) = &credential_scope {
        scope.apply_env(&mut child_env);
    }
    let mut error_redactions = credential_env.redaction_values();
    if let Some(scope) = &credential_scope {
        error_redactions.extend(scope.redaction_values());
    }
    let artifact_root = artifact_root(handle);
    let work_root = artifact_root
        .join("work")
        .join(safe_segment(handle.id.as_str()))
        .join(safe_segment(request_id.as_str()));
    let allowed_commands = command.clone().into_iter().collect::<Vec<_>>();
    let config = SkillRunnerConfig::new(
        allowed_commands,
        timeout_ms(handle),
        output_limit_bytes(handle),
        artifact_root,
        work_root,
    );
    let run_result = SkillRunner.run_with_spawner_as(
        &config,
        SkillRunnerInvocation {
            adapter_id: handle.id.clone(),
            request_id: request_id.clone(),
            skill_id: skill.to_owned(),
            entry_type,
            command,
            args,
            env: child_env.clone(),
            input: invocation.input,
        },
        process_spawner,
        &handle.provider.run_as,
    );
    child_env.clear();
    let run = match (run_result, credential_env.release()) {
        (Err(error), _) => return Err(sanitize_error_with_values(error, &error_redactions)),
        (Ok(_), Err(error)) => {
            return Err(sanitize_error_with_values(error, &error_redactions));
        }
        (Ok(run), Ok(())) => run,
    };
    let credential_audit = credential_env.audit_entries();

    let mut audit = vec![format!("adapter.invoked:{}", handle.id.as_str())];
    audit.extend(run.audit.clone());
    audit.extend(credential_audit);
    if let Some(scope) = &credential_scope {
        audit.extend(scope.audit_entries());
    }
    audit.push(format!("skill.status:{}", run.status.as_str()));
    audit.push(format!(
        "skill.artifacts:{}",
        run.artifacts
            .iter()
            .map(|artifact| artifact.key.as_str())
            .collect::<Vec<_>>()
            .join(",")
    ));

    Ok(AdapterInvokeReport {
        request_id,
        adapter_id: handle.id.clone(),
        transport: handle.transport,
        capability,
        status: run.status.as_str().to_owned(),
        output: skill_output_json(skill, &run),
        audit,
        trace,
    })
}

/// 校验 `validate_runner_config` 对应的约束，不满足时返回明确错误。
fn validate_runner_config(
    config: &SkillRunnerConfig,
    invocation: &SkillRunnerInvocation,
) -> Result<(), EvaError> {
    if invocation.skill_id.trim().is_empty() {
        return Err(EvaError::invalid_argument("skill id cannot be empty"));
    }
    if let Some(command) = &invocation.command {
        if command.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "skill runner command cannot be empty",
            ));
        }
        if !config.allowed_commands.contains(command) {
            return Err(
                EvaError::permission_denied("skill runner command is not allowlisted")
                    .with_context("command", command),
            );
        }
    }
    if config.output_limit_bytes == 0 {
        return Err(EvaError::invalid_argument(
            "skill output limit must be greater than zero",
        ));
    }
    Ok(())
}

/// 创建调用专属的工作和制品目录，并把原始输入作为可追踪证据写入工作目录。
fn prepare_run_paths(
    config: &SkillRunnerConfig,
    invocation: &SkillRunnerInvocation,
    sensitive_values: &[String],
) -> Result<RunPaths, EvaError> {
    ensure_skill_directory_tree(&config.artifact_root, "skill artifact root").map_err(|error| {
        EvaError::permission_denied("skill artifact root is not a controlled directory")
            .with_context("adapter_id", invocation.adapter_id.as_str())
            .with_context("path", config.artifact_root.display().to_string())
            .with_context("io_error", error.to_string())
    })?;

    let working_dir = config.work_root.clone();
    ensure_skill_path_within_root(&config.artifact_root, &working_dir, "skill work directory")
        .map_err(|error| {
            EvaError::permission_denied("skill work directory escaped the artifact root")
                .with_context("adapter_id", invocation.adapter_id.as_str())
                .with_context("path", working_dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
    ensure_skill_directory_tree(&working_dir, "skill work directory").map_err(|error| {
        EvaError::permission_denied("skill work directory is not controlled")
            .with_context("adapter_id", invocation.adapter_id.as_str())
            .with_context("path", working_dir.display().to_string())
            .with_context("io_error", error.to_string())
    })?;

    let artifact_dir = working_dir.join("artifacts");
    ensure_skill_directory_tree(&artifact_dir, "skill artifact directory").map_err(|error| {
        EvaError::permission_denied("skill artifact directory is not controlled")
            .with_context("adapter_id", invocation.adapter_id.as_str())
            .with_context("path", artifact_dir.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    let input_path = working_dir.join("input.json");
    let redacted_input =
        redact_provider_stream_bytes(invocation.input.as_bytes().to_vec(), sensitive_values);
    write_skill_file_no_follow(
        &working_dir,
        &input_path,
        &redacted_input,
        "skill input evidence",
    )
    .map_err(|error| {
        EvaError::internal("failed to write skill input evidence")
            .with_context("adapter_id", invocation.adapter_id.as_str())
            .with_context("path", input_path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    Ok(RunPaths {
        working_dir,
        artifact_dir,
        artifact_root: config.artifact_root.clone(),
    })
}

/// Create a directory tree without following symlink/reparse-point components.
/// Existing ancestors are checked before each create, and the final directory
/// must be owned by the current daemon identity.
fn ensure_skill_directory_tree(path: &Path, purpose: &str) -> io::Result<()> {
    if path.as_os_str().is_empty() {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!("{purpose} path is empty"),
        ));
    }

    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!("{purpose} path contains a parent component"),
        ));
    }

    // Walk existing ancestors using the original PathBuf. This matters for
    // Windows verbatim paths, whose Prefix/RootDir components must not be
    // reconstructed by repeated push operations.
    let mut missing = Vec::new();
    let mut cursor = path.to_path_buf();
    loop {
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) => {
                ensure_skill_directory_metadata(&metadata, &cursor, purpose)?;
                break;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                missing.push(cursor.clone());
                let Some(parent) = cursor.parent() else {
                    return Err(error);
                };
                if parent == cursor {
                    return Err(error);
                }
                cursor = parent.to_path_buf();
            }
            Err(error) => return Err(error),
        }
    }

    for directory in missing.iter().rev() {
        match fs::create_dir(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let metadata = fs::symlink_metadata(directory)?;
        ensure_skill_directory_metadata(&metadata, directory, purpose)?;
    }

    let metadata = fs::symlink_metadata(path)?;
    ensure_skill_directory_metadata(&metadata, path, purpose)?;
    ensure_skill_directory_owner(&metadata, path, purpose)
}

/// Verify that a path is lexically below its configured artifact root.
fn ensure_skill_path_within_root(root: &Path, path: &Path, purpose: &str) -> io::Result<()> {
    let relative = path.strip_prefix(root).map_err(|_| {
        io::Error::new(
            ErrorKind::InvalidInput,
            format!("{purpose} is outside its configured root"),
        )
    })?;
    if relative
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!("{purpose} contains a parent component"),
        ));
    }
    Ok(())
}

fn ensure_skill_directory_metadata(
    metadata: &fs::Metadata,
    path: &Path,
    purpose: &str,
) -> io::Result<()> {
    if metadata_is_skill_link_or_reparse(metadata) {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            format!(
                "{purpose} contains a symlink or reparse point: {}",
                path.display()
            ),
        ));
    }
    if !metadata.file_type().is_dir() {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            format!("{purpose} is not a directory: {}", path.display()),
        ));
    }

    // A writable non-sticky ancestor lets another user replace a checked
    // component between directory creation and the provider spawn.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = metadata.mode();
        if mode & 0o022 != 0 && mode & 0o1000 == 0 {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                format!(
                    "{purpose} ancestor is group/other writable: {}",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

fn ensure_skill_directory_owner(
    metadata: &fs::Metadata,
    path: &Path,
    purpose: &str,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let expected_uid = unsafe { libc::geteuid() } as u32;
        let expected_gid = unsafe { libc::getegid() } as u32;
        if metadata.uid() != expected_uid || metadata.gid() != expected_gid {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                format!(
                    "{purpose} owner does not match the daemon identity: {}",
                    path.display()
                ),
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = (metadata, path, purpose);
    Ok(())
}

fn ensure_skill_path_ancestors(root: &Path, path: &Path, purpose: &str) -> io::Result<()> {
    ensure_skill_path_within_root(root, path, purpose)?;
    let relative = path.strip_prefix(root).map_err(|_| {
        io::Error::new(
            ErrorKind::InvalidInput,
            format!("{purpose} is outside its configured root"),
        )
    })?;
    let mut cursor = root.to_path_buf();
    let components = relative.components().collect::<Vec<_>>();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        let Component::Normal(segment) = component else {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                format!("{purpose} contains an unsupported path component"),
            ));
        };
        cursor.push(segment);
        let metadata = fs::symlink_metadata(&cursor)?;
        ensure_skill_directory_metadata(&metadata, &cursor, purpose)?;
        ensure_skill_directory_owner(&metadata, &cursor, purpose)?;
    }
    let root_metadata = fs::symlink_metadata(root)?;
    ensure_skill_directory_metadata(&root_metadata, root, purpose)?;
    ensure_skill_directory_owner(&root_metadata, root, purpose)
}

fn write_skill_file_no_follow(
    root: &Path,
    path: &Path,
    bytes: &[u8],
    purpose: &str,
) -> io::Result<()> {
    ensure_skill_path_ancestors(root, path, purpose)?;
    if let Ok(metadata) = fs::symlink_metadata(path) {
        ensure_skill_regular_file_metadata(&metadata, path, purpose)?;
        ensure_skill_file_owner(&metadata, path, purpose)?;
    }

    let mut options = fs::OpenOptions::new();
    // Delay truncation until the opened handle has passed type and ownership
    // checks; otherwise a foreign regular file could be destroyed first.
    options.write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        options.custom_flags(0x0002_0000); // O_NOFOLLOW
        #[cfg(target_os = "macos")]
        options.custom_flags(0x0000_0100); // O_NOFOLLOW
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let mut file = options.open(path)?;
    let metadata = file.metadata()?;
    ensure_skill_regular_file_metadata(&metadata, path, purpose)?;
    ensure_skill_file_owner(&metadata, path, purpose)?;
    file.set_len(0)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)?;
    file.sync_all()
}

fn open_skill_file_no_follow(root: &Path, path: &Path, purpose: &str) -> io::Result<fs::File> {
    ensure_skill_path_ancestors(root, path, purpose)?;
    let metadata = fs::symlink_metadata(path)?;
    ensure_skill_regular_file_metadata(&metadata, path, purpose)?;

    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        options.custom_flags(0x0002_0000); // O_NOFOLLOW
        #[cfg(target_os = "macos")]
        options.custom_flags(0x0000_0100); // O_NOFOLLOW
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path)?;
    let handle_metadata = file.metadata()?;
    ensure_skill_regular_file_metadata(&handle_metadata, path, purpose)?;
    ensure_skill_file_owner(&handle_metadata, path, purpose)?;
    Ok(file)
}

fn ensure_skill_regular_file_metadata(
    metadata: &fs::Metadata,
    path: &Path,
    purpose: &str,
) -> io::Result<()> {
    if metadata_is_skill_link_or_reparse(metadata) || !metadata.file_type().is_file() {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            format!("{purpose} is not a regular file: {}", path.display()),
        ));
    }
    Ok(())
}

fn ensure_skill_file_owner(metadata: &fs::Metadata, path: &Path, purpose: &str) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let expected_uid = unsafe { libc::geteuid() } as u32;
        if metadata.uid() != expected_uid {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                format!(
                    "{purpose} owner does not match the daemon identity: {}",
                    path.display()
                ),
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = (metadata, path, purpose);
    Ok(())
}

#[cfg(windows)]
fn metadata_is_skill_link_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_skill_link_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

/// 执行 `run_builtin_codex_skill` 对应的受控流程。
fn run_builtin_codex_skill(
    paths: &RunPaths,
    invocation: &SkillRunnerInvocation,
    sensitive_values: &[String],
) -> Result<RawSkillRunReport, EvaError> {
    let started_at = Instant::now();
    let redacted_input = String::from_utf8_lossy(&redact_provider_stream_bytes(
        invocation.input.as_bytes().to_vec(),
        sensitive_values,
    ))
    .into_owned();
    let result = format!(
        "{{\"summary\":\"controlled workflow skill completed\",\"findings\":[],\"skill_id\":{},\"input\":{}}}",
        json_string(&invocation.skill_id),
        redacted_input
    );
    let result_path = paths.artifact_dir.join("result.json");
    write_skill_file_no_follow(
        &paths.artifact_dir,
        &result_path,
        result.as_bytes(),
        "built-in skill result artifact",
    )
    .map_err(|error| {
        EvaError::internal("failed to write built-in skill result artifact")
            .with_context("skill_id", &invocation.skill_id)
            .with_context("path", result_path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    let stdout_stream = capture_provider_bytes(
        ProviderStreamConfig::new("stdout", result.len().max(1)),
        result.clone().into_bytes(),
        1,
        false,
        sensitive_values,
    )?;
    let stderr_stream = ProviderStreamCapture::empty("stderr");
    Ok(RawSkillRunReport {
        runner: "builtin_codex_skill".to_owned(),
        status: SkillRunStatus::Completed,
        exit_code: Some(0),
        stdout: stdout_stream.preview.clone(),
        stderr: Vec::new(),
        stdout_stream,
        stderr_stream,
        duration_ms: started_at.elapsed().as_millis(),
        audit: vec![
            "transport:skill".to_owned(),
            "skill.runner:builtin_codex_skill".to_owned(),
            "skill.working_dir:isolated".to_owned(),
            "skill.artifact_dir:controlled".to_owned(),
        ],
    })
}

/// 不经 shell 启动技能命令，并发排空两路输出；超时或任一路触限都会回收子进程。
fn run_process(
    config: &SkillRunnerConfig,
    paths: &RunPaths,
    invocation: &SkillRunnerInvocation,
    command: &str,
    sensitive_values: &[String],
    process_spawner: &dyn ProviderProcessSpawner,
    run_as: &ProviderRunAsIdentity,
) -> Result<RawSkillRunReport, EvaError> {
    let started_at = Instant::now();
    let mut env_values = invocation.env.clone();
    env_values.insert(
        "EVA_SKILL_ARTIFACT_DIR".to_owned(),
        paths.artifact_dir.display().to_string(),
    );
    env_values.insert(
        "EVA_SKILL_WORKDIR".to_owned(),
        paths.working_dir.display().to_string(),
    );
    env_values.insert(
        "EVA_SKILL_INPUT_PATH".to_owned(),
        paths.working_dir.join("input.json").display().to_string(),
    );
    env_values.insert("EVA_SKILL_ID".to_owned(), invocation.skill_id.clone());
    env_values.insert(
        "EVA_REQUEST_ID".to_owned(),
        invocation.request_id.as_str().to_owned(),
    );
    // `command` 已命中白名单，参数按独立 argv 传递，不解释管道、重定向或变量展开。
    let mut command_line = Command::new(command);
    command_line
        .args(&invocation.args)
        .env_clear()
        .envs(minimal_process_env(&env_values))
        .current_dir(&paths.working_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = SkillChild::Supervised(
        process_spawner
            .spawn_provider_as(command_line, run_as)
            .map_err(|error| error.with_context("command", command))?,
    );
    let process_id = child.pid();

    if !invocation.input.is_empty() {
        let Some(mut stdin) = child.take_stdin() else {
            terminate_skill_child(&mut child);
            return Err(EvaError::internal("skill runner stdin was not available")
                .with_context("command", command));
        };
        if let Err(error) = stdin.write_all(invocation.input.as_bytes()) {
            drop(stdin);
            terminate_skill_child(&mut child);
            return Err(EvaError::unavailable("failed to write skill runner input")
                .with_context("command", command)
                .with_context("io_error", error.to_string()));
        }
    }
    drop(child.take_stdin());

    let Some(stdout) = child.take_stdout() else {
        terminate_skill_child(&mut child);
        return Err(EvaError::internal("skill runner stdout was not available")
            .with_context("command", command));
    };
    let Some(stderr) = child.take_stderr() else {
        terminate_skill_child(&mut child);
        return Err(EvaError::internal("skill runner stderr was not available")
            .with_context("command", command));
    };
    let (sender, receiver) = mpsc::channel();
    spawn_reader(
        skill_stream_config(config, invocation, "stdout"),
        stdout,
        sensitive_values.to_vec(),
        sender.clone(),
    );
    spawn_reader(
        skill_stream_config(config, invocation, "stderr"),
        stderr,
        sensitive_values.to_vec(),
        sender,
    );

    let timeout = Duration::from_millis(config.timeout_ms);
    let mut stdout_capture = ProviderStreamCapture::empty("stdout");
    let mut stderr_capture = ProviderStreamCapture::empty("stderr");
    let mut received_streams = 0;
    while received_streams < 2 {
        let message = if timeout.is_zero() {
            receiver.recv().map_err(mpsc::RecvTimeoutError::from)
        } else {
            let elapsed = started_at.elapsed();
            if elapsed >= timeout {
                terminate_skill_child(&mut child);
                return Ok(raw_process_report(
                    command,
                    process_id,
                    SkillRunStatus::Timeout,
                    None,
                    stdout_capture,
                    stderr_capture,
                    started_at,
                ));
            }
            receiver.recv_timeout(timeout - elapsed)
        };
        match message {
            Ok(ReaderMessage::Output { capture }) => {
                received_streams += 1;
                let truncated = capture.truncated;
                if capture.stream_name == "stdout" {
                    stdout_capture = capture;
                } else {
                    stderr_capture = capture;
                }
                if truncated {
                    terminate_skill_child(&mut child);
                    return Ok(raw_process_report(
                        command,
                        process_id,
                        SkillRunStatus::OutputLimitExceeded,
                        None,
                        stdout_capture,
                        stderr_capture,
                        started_at,
                    ));
                }
            }
            Ok(ReaderMessage::ReadError { stream, error }) => {
                terminate_skill_child(&mut child);
                return Err(EvaError::unavailable("failed to read skill runner output")
                    .with_context("command", command)
                    .with_context("stream", stream)
                    .with_context("io_error", error));
            }
            Err(_) => {
                terminate_skill_child(&mut child);
                return Ok(raw_process_report(
                    command,
                    process_id,
                    SkillRunStatus::Timeout,
                    None,
                    stdout_capture,
                    stderr_capture,
                    started_at,
                ));
            }
        }
    }

    loop {
        if let Some(status) = child.try_wait()? {
            let exit_code = status.code();
            let run_status = if exit_code == Some(0) {
                SkillRunStatus::Completed
            } else {
                SkillRunStatus::Failed
            };
            return Ok(raw_process_report(
                command,
                process_id,
                run_status,
                exit_code,
                stdout_capture,
                stderr_capture,
                started_at,
            ));
        }
        if !timeout.is_zero() && started_at.elapsed() >= timeout {
            terminate_skill_child(&mut child);
            return Ok(raw_process_report(
                command,
                process_id,
                SkillRunStatus::Timeout,
                None,
                stdout_capture,
                stderr_capture,
                started_at,
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// 执行 `raw_process_report` 对应的处理逻辑。
fn raw_process_report(
    command: &str,
    process_id: u32,
    status: SkillRunStatus,
    exit_code: Option<i32>,
    stdout_stream: ProviderStreamCapture,
    stderr_stream: ProviderStreamCapture,
    started_at: Instant,
) -> RawSkillRunReport {
    let stdout = stdout_stream.preview.clone();
    let stderr = stderr_stream.preview.clone();
    let mut audit = vec![
        "transport:skill".to_owned(),
        "skill.runner:process".to_owned(),
        "shell:false".to_owned(),
        format!("skill.command:{command}"),
        format!("process_id:{process_id}"),
    ];
    audit.extend(provider_stream_audit(&stdout_stream));
    audit.extend(provider_stream_audit(&stderr_stream));
    RawSkillRunReport {
        runner: "process".to_owned(),
        status,
        exit_code,
        stdout,
        stderr,
        stdout_stream,
        stderr_stream,
        duration_ms: started_at.elapsed().as_millis(),
        audit,
    }
}

/// 持久化标准流、运行报告和技能声明制品，返回的证据只包含已成功写入的记录。
fn persist_run_artifacts(
    paths: &RunPaths,
    invocation: &SkillRunnerInvocation,
    raw: &RawSkillRunReport,
    sensitive_values: &[String],
) -> Result<Vec<SkillArtifactEvidence>, EvaError> {
    let mut store = FileSystemArtifactStore::new(&paths.artifact_root);
    let base_key = format!(
        "skill/{}/{}",
        safe_segment(invocation.adapter_id.as_str()),
        safe_segment(invocation.request_id.as_str())
    );
    let mut artifacts = Vec::new();
    if let Some(artifact) = &raw.stdout_stream.artifact {
        artifacts.push(evidence_from_stream_artifact(artifact));
    } else {
        artifacts.push(evidence_from_record(store.put_bytes_with_metadata(
            format!("{base_key}/stdout"),
            raw.stdout.clone(),
            "text/plain",
            "retain",
            None,
        )?));
    }
    if let Some(artifact) = &raw.stderr_stream.artifact {
        artifacts.push(evidence_from_stream_artifact(artifact));
    } else {
        artifacts.push(evidence_from_record(store.put_bytes_with_metadata(
            format!("{base_key}/stderr"),
            raw.stderr.clone(),
            "text/plain",
            "retain",
            None,
        )?));
    }
    artifacts.push(evidence_from_record(store.put_bytes_with_metadata(
        format!("{base_key}/run-report"),
        run_report_artifact_json(raw).into_bytes(),
        "application/json",
        "retain",
        None,
    )?));

    if paths.artifact_dir.exists() {
        collect_artifact_dir(
            &mut store,
            &paths.artifact_dir,
            &paths.artifact_dir,
            &base_key,
            sensitive_values,
            &mut artifacts,
        )?;
    }
    Ok(artifacts)
}

/// 递归收集普通文件；拒绝符号链接并对每个文件脱敏，防止越界读取和凭据泄露。
fn collect_artifact_dir(
    store: &mut FileSystemArtifactStore,
    root_dir: &Path,
    artifact_dir: &Path,
    base_key: &str,
    sensitive_values: &[String],
    artifacts: &mut Vec<SkillArtifactEvidence>,
) -> Result<(), EvaError> {
    let directory_metadata = fs::symlink_metadata(artifact_dir).map_err(|error| {
        EvaError::internal("failed to read skill artifact directory metadata")
            .with_context("path", artifact_dir.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    ensure_skill_directory_metadata(
        &directory_metadata,
        artifact_dir,
        "skill artifact directory",
    )
    .map_err(|error| {
        EvaError::permission_denied("skill artifact directory is not controlled")
            .with_context("path", artifact_dir.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    for entry in fs::read_dir(artifact_dir).map_err(|error| {
        EvaError::internal("failed to read skill artifact directory")
            .with_context("path", artifact_dir.display().to_string())
            .with_context("io_error", error.to_string())
    })? {
        let entry = entry.map_err(|error| {
            EvaError::internal("failed to read skill artifact entry")
                .with_context("path", artifact_dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        ensure_skill_directory_owner(
            &directory_metadata,
            artifact_dir,
            "skill artifact directory",
        )
        .map_err(|error| {
            EvaError::permission_denied("skill artifact directory owner is not controlled")
                .with_context("path", artifact_dir.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            EvaError::internal("failed to read skill artifact metadata")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        if metadata_is_skill_link_or_reparse(&metadata) {
            // 即使链接目标仍在目录内也统一拒绝，避免检查与读取之间的目标替换竞态。
            return Err(
                EvaError::permission_denied("skill artifact symlink is not allowed")
                    .with_context("path", path.display().to_string()),
            );
        }
        if metadata.is_dir() {
            collect_artifact_dir(
                store,
                root_dir,
                &path,
                base_key,
                sensitive_values,
                artifacts,
            )?;
        } else if metadata.is_file() {
            let relative = path.strip_prefix(root_dir).map_err(|error| {
                EvaError::internal("failed to compute skill artifact relative path")
                    .with_context("path", path.display().to_string())
                    .with_context("path_error", error.to_string())
            })?;
            let relative_key = relative_artifact_key(relative)?;
            // Read through one no-follow handle so a replacement between
            // metadata inspection and fs::read cannot redirect the read.
            let mut file = open_skill_file_no_follow(root_dir, &path, "skill artifact file")
                .map_err(|error| {
                    EvaError::permission_denied("skill artifact file is not controlled")
                        .with_context("path", path.display().to_string())
                        .with_context("io_error", error.to_string())
                })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(0o600))
                    .map_err(|error| {
                        EvaError::permission_denied("skill artifact permissions are not controlled")
                            .with_context("path", path.display().to_string())
                            .with_context("io_error", error.to_string())
                    })?;
            }
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).map_err(|error| {
                EvaError::internal("failed to read skill artifact")
                    .with_context("path", path.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
            let bytes = redact_provider_stream_bytes(bytes, sensitive_values);
            artifacts.push(evidence_from_record(store.put_bytes_with_metadata(
                format!("{base_key}/artifacts/{relative_key}"),
                bytes,
                "application/octet-stream",
                "retain",
                None,
            )?));
        }
    }
    Ok(())
}

/// 将相对路径转换为制品键，只接受普通且可移植的安全路径段。
fn relative_artifact_key(relative: &Path) -> Result<String, EvaError> {
    let mut segments = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(segment) => {
                let segment = segment.to_string_lossy();
                if segment.trim().is_empty()
                    || segment == "."
                    || segment == ".."
                    || !segment.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
                {
                    return Err(EvaError::invalid_argument(
                        "skill artifact path is not controlled",
                    )
                    .with_context("artifact_path", relative.display().to_string()));
                }
                segments.push(segment.into_owned());
            }
            _ => {
                return Err(
                    EvaError::invalid_argument("skill artifact path is not controlled")
                        .with_context("artifact_path", relative.display().to_string()),
                );
            }
        }
    }
    if segments.is_empty() {
        return Err(EvaError::invalid_argument("skill artifact path is empty"));
    }
    Ok(segments.join("/"))
}

/// 执行 `evidence_from_record` 对应的处理逻辑。
fn evidence_from_record(record: ArtifactRecord) -> SkillArtifactEvidence {
    SkillArtifactEvidence {
        key: record.key,
        digest: record.digest,
        size_bytes: record.size_bytes,
        content_type: record.content_type,
    }
}

/// 执行 `evidence_from_stream_artifact` 对应的处理逻辑。
fn evidence_from_stream_artifact(artifact: &ProviderStreamArtifact) -> SkillArtifactEvidence {
    SkillArtifactEvidence {
        key: artifact.key.clone(),
        digest: artifact.digest.clone(),
        size_bytes: artifact.size_bytes,
        content_type: artifact.content_type.clone(),
    }
}

/// 执行 `provider_stream_artifact_from_skill_evidence` 对应的处理逻辑。
fn provider_stream_artifact_from_skill_evidence(
    evidence: &SkillArtifactEvidence,
) -> ProviderStreamArtifact {
    ProviderStreamArtifact {
        key: evidence.key.clone(),
        digest: evidence.digest.clone(),
        size_bytes: evidence.size_bytes,
        content_type: evidence.content_type.clone(),
    }
}

/// 校验 `validate_skill_input` 对应的约束，不满足时返回明确错误。
fn validate_skill_input(handle: &AdapterHandle, input: &str) -> Result<(), EvaError> {
    let schema = handle.skill_input_schema.as_ref().ok_or_else(|| {
        EvaError::permission_denied("Skill adapter is missing input schema")
            .with_context("adapter_id", handle.id.as_str())
    })?;
    if schema.schema_type.as_deref() != Some("object") {
        return Err(
            EvaError::unsupported("Skill input schema type is unsupported")
                .with_context("adapter_id", handle.id.as_str())
                .with_context("schema_type", schema.schema_type.as_deref().unwrap_or("")),
        );
    }
    let object = parse_json_object(input).map_err(|error| {
        error
            .with_context("adapter_id", handle.id.as_str())
            .with_context("schema", "skill.input_schema")
    })?;
    validate_object_schema(schema, &object).map_err(|error| {
        error
            .with_context("adapter_id", handle.id.as_str())
            .with_context("schema", "skill.input_schema")
    })
}

/// 校验 `validate_object_schema` 对应的约束，不满足时返回明确错误。
fn validate_object_schema(
    schema: &SkillInputSchema,
    object: &BTreeMap<String, ParsedJsonValue>,
) -> Result<(), EvaError> {
    for required in &schema.required {
        if !object.contains_key(required) {
            return Err(
                EvaError::invalid_argument("Skill input is missing required field")
                    .with_context("field", required),
            );
        }
    }
    for (name, property) in &schema.properties {
        let Some(value) = object.get(name) else {
            continue;
        };
        if property.value_type.as_deref() == Some("string") {
            let ParsedJsonValue::String(value) = value else {
                return Err(
                    EvaError::invalid_argument("Skill input field has invalid type")
                        .with_context("field", name)
                        .with_context("expected", "string"),
                );
            };
            if !property.enum_values.is_empty() && !property.enum_values.contains(value) {
                return Err(EvaError::invalid_argument(
                    "Skill input field enum value is not allowed",
                )
                .with_context("field", name)
                .with_context("value", value));
            }
        }
    }
    Ok(())
}

/// 读取或解析 `parse_json_object` 所需的数据，失败时保留错误语义。
fn parse_json_object(input: &str) -> Result<BTreeMap<String, ParsedJsonValue>, EvaError> {
    let mut parser = JsonObjectParser::new(input);
    parser.parse_object()
}

/// 表示 `JsonObjectParser` 数据结构。
struct JsonObjectParser<'a> {
    /// 记录 `input` 字段对应的值。
    input: &'a str,
    /// 记录 `chars` 字段对应的值。
    chars: Vec<char>,
    /// 记录 `index` 字段对应的值。
    index: usize,
}

impl<'a> JsonObjectParser<'a> {
    /// 创建并初始化当前类型的实例。
    fn new(input: &'a str) -> Self {
        Self {
            input,
            chars: input.chars().collect(),
            index: 0,
        }
    }

    /// 读取或解析 `parse_object` 所需的数据，失败时保留错误语义。
    fn parse_object(&mut self) -> Result<BTreeMap<String, ParsedJsonValue>, EvaError> {
        self.skip_whitespace();
        self.expect('{')?;
        let mut object = BTreeMap::new();
        loop {
            self.skip_whitespace();
            if self.consume('}') {
                break;
            }
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect(':')?;
            self.skip_whitespace();
            let value = if self.peek() == Some('"') {
                ParsedJsonValue::String(self.parse_string()?)
            } else {
                self.consume_non_string_value()?;
                ParsedJsonValue::Other
            };
            object.insert(key, value);
            self.skip_whitespace();
            if self.consume('}') {
                break;
            }
            self.expect(',')?;
        }
        self.skip_whitespace();
        if self.index != self.chars.len() {
            return Err(EvaError::invalid_argument(
                "Skill input must be a JSON object",
            ));
        }
        Ok(object)
    }

    /// 读取或解析 `parse_string` 所需的数据，失败时保留错误语义。
    fn parse_string(&mut self) -> Result<String, EvaError> {
        self.expect('"')?;
        let mut value = String::new();
        while let Some(character) = self.next() {
            match character {
                '"' => return Ok(value),
                '\\' => {
                    let escaped = self.next().ok_or_else(|| {
                        EvaError::invalid_argument("Skill input contains an invalid JSON escape")
                    })?;
                    match escaped {
                        '"' => value.push('"'),
                        '\\' => value.push('\\'),
                        '/' => value.push('/'),
                        'b' => value.push('\u{0008}'),
                        'f' => value.push('\u{000c}'),
                        'n' => value.push('\n'),
                        'r' => value.push('\r'),
                        't' => value.push('\t'),
                        'u' => {
                            for _ in 0..4 {
                                if self.next().is_none() {
                                    return Err(EvaError::invalid_argument(
                                        "Skill input contains an invalid unicode escape",
                                    ));
                                }
                            }
                            value.push('?');
                        }
                        _ => {
                            return Err(EvaError::invalid_argument(
                                "Skill input contains an unsupported JSON escape",
                            ));
                        }
                    }
                }
                value_char => value.push(value_char),
            }
        }
        Err(EvaError::invalid_argument(
            "Skill input contains an unterminated JSON string",
        ))
    }

    /// 执行 `consume_non_string_value` 对应的处理逻辑。
    fn consume_non_string_value(&mut self) -> Result<(), EvaError> {
        let start = self.index;
        let mut nested_depth = 0_i32;
        let mut in_string = false;
        let mut escaped = false;
        while let Some(character) = self.peek() {
            if in_string {
                self.index += 1;
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    in_string = false;
                }
                continue;
            }
            match character {
                '"' => {
                    in_string = true;
                    self.index += 1;
                }
                '[' | '{' => {
                    nested_depth += 1;
                    self.index += 1;
                }
                ']' | '}' if nested_depth > 0 => {
                    nested_depth -= 1;
                    self.index += 1;
                }
                ',' | '}' if nested_depth == 0 => break,
                _ => self.index += 1,
            }
        }
        if self.input[start..self.byte_index()].trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "Skill input field value is empty",
            ));
        }
        Ok(())
    }

    /// 执行 `skip_whitespace` 对应的处理逻辑。
    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(value) if value.is_whitespace()) {
            self.index += 1;
        }
    }

    /// 执行 `expect` 对应的处理逻辑。
    fn expect(&mut self, expected: char) -> Result<(), EvaError> {
        if self.consume(expected) {
            Ok(())
        } else {
            Err(
                EvaError::invalid_argument("Skill input must be a JSON object")
                    .with_context("expected", expected.to_string()),
            )
        }
    }

    /// 执行 `consume` 对应的处理逻辑。
    fn consume(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    /// 执行 `peek` 对应的处理逻辑。
    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).copied()
    }

    /// 执行 `next` 对应的处理逻辑。
    fn next(&mut self) -> Option<char> {
        let value = self.peek()?;
        self.index += 1;
        Some(value)
    }

    /// 执行 `byte_index` 对应的处理逻辑。
    fn byte_index(&self) -> usize {
        self.chars
            .iter()
            .take(self.index)
            .map(|character| character.len_utf8())
            .sum()
    }
}

/// 定义 `ReaderMessage` 可取的状态。
enum ReaderMessage {
    /// 表示 `Output` 枚举分支。
    Output {
        /// 已完成脱敏和截断处理的单个 runner 输出流捕获结果。
        capture: ProviderStreamCapture,
    },
    /// 表示 `ReadError` 枚举分支。
    ReadError {
        /// 读取失败的 runner 输出流名称，例如 `stdout` 或 `stderr`。
        stream: String,
        /// 底层读取错误转换得到的可审计文本。
        error: String,
    },
}

/// 执行 `spawn_reader` 对应的受控流程。
fn spawn_reader(
    config: ProviderStreamConfig,
    reader: impl Read + Send + 'static,
    sensitive_values: Vec<String>,
    sender: mpsc::Sender<ReaderMessage>,
) {
    thread::spawn(move || {
        let stream = config.stream_name.clone();
        match collect_provider_stream(reader, config, &sensitive_values) {
            Ok(capture) => {
                let _ = sender.send(ReaderMessage::Output { capture });
            }
            Err(error) => {
                let _ = sender.send(ReaderMessage::ReadError {
                    stream,
                    error: error.to_string(),
                });
            }
        }
    });
}

/// 执行 `skill_stream_config` 对应的处理逻辑。
fn skill_stream_config(
    config: &SkillRunnerConfig,
    invocation: &SkillRunnerInvocation,
    stream_name: &str,
) -> ProviderStreamConfig {
    ProviderStreamConfig::new(stream_name, config.output_limit_bytes)
        .with_preview_limit(config.preview_limit_bytes)
        .with_chunk_size(config.stream_chunk_size_bytes)
        .with_artifact(
            config.artifact_root.clone(),
            provider_stream_key(
                "skill",
                invocation.adapter_id.as_str(),
                invocation.request_id.as_str(),
                stream_name,
            ),
            "text/plain",
        )
}

/// 校验 `validate_input_size` 对应的约束，不满足时返回明确错误。
fn validate_input_size(handle: &AdapterHandle, input: &str) -> Result<(), EvaError> {
    if let Some(limit) = handle.max_prompt_bytes {
        if input.len() > limit {
            return Err(EvaError::conflict("Skill input exceeded prompt limit")
                .with_context("adapter_id", handle.id.as_str())
                .with_context("max_prompt_bytes", limit.to_string())
                .with_context("actual_bytes", input.len().to_string()));
        }
    }
    Ok(())
}

/// 执行 `timeout_ms` 对应的处理逻辑。
fn timeout_ms(handle: &AdapterHandle) -> u64 {
    handle.timeout_ms.unwrap_or(30_000)
}

/// 执行 `output_limit_bytes` 对应的处理逻辑。
fn output_limit_bytes(handle: &AdapterHandle) -> usize {
    handle
        .output_limit_bytes
        .or(handle.max_prompt_bytes)
        .unwrap_or(64 * 1024)
}

/// 执行 `artifact_root` 对应的处理逻辑。
fn artifact_root(handle: &AdapterHandle) -> PathBuf {
    if let Some(root) = &handle.skill_artifact_root {
        return expand_home(root);
    }
    let source_path = PathBuf::from(&handle.source_path);
    if let Some(project_root) = project_root_from_manifest_path(&source_path) {
        return project_root.join(".eva").join("artifacts");
    }
    env::temp_dir().join("eva-skill-artifacts")
}

/// 执行 `project_root_from_manifest_path` 对应的处理逻辑。
fn project_root_from_manifest_path(path: &Path) -> Option<PathBuf> {
    let config_dir = path.parent()?.parent()?;
    if config_dir.file_name().and_then(|value| value.to_str()) == Some("config") {
        return config_dir.parent().map(Path::to_path_buf);
    }
    None
}

/// 执行 `expand_home` 对应的处理逻辑。
fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// 执行 `sensitive_values` 对应的处理逻辑。
fn sensitive_values<'a>(values: impl IntoIterator<Item = &'a String>) -> Vec<String> {
    values
        .into_iter()
        .filter(|value| !value.is_empty())
        .cloned()
        .collect()
}

/// 执行 `safe_segment` 对应的处理逻辑。
fn safe_segment(value: &str) -> String {
    let mut segment = value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                char::from(byte)
            } else {
                '_'
            }
        })
        .collect::<String>();
    if segment.trim_matches('_').is_empty() {
        segment = "unknown".to_owned();
    }
    segment
}

/// 执行 `skill_output_json` 对应的处理逻辑。
fn skill_output_json(skill: &str, run: &SkillRunReport) -> String {
    format!(
        "{{\"transport\":\"skill\",\"skill\":{},\"runner\":{},\"status\":{},\"exit_code\":{},\"stdout\":{},\"stderr\":{},\"duration_ms\":{},\"working_dir\":{},\"artifact_root\":{},\"artifacts\":{}}}",
        json_string(skill),
        json_string(&run.runner),
        json_string(run.status.as_str()),
        run.exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "null".to_owned()),
        provider_stream_summary_json(&run.stdout_stream),
        provider_stream_summary_json(&run.stderr_stream),
        run.duration_ms,
        json_string(&run.working_dir.display().to_string()),
        json_string(&run.artifact_root.display().to_string()),
        json_array(run.artifacts.iter().map(artifact_json))
    )
}

/// 执行 `artifact_json` 对应的处理逻辑。
fn artifact_json(artifact: &SkillArtifactEvidence) -> String {
    format!(
        "{{\"key\":{},\"digest\":{},\"size_bytes\":{},\"content_type\":{}}}",
        json_string(&artifact.key),
        json_string(&artifact.digest),
        artifact.size_bytes,
        json_string(&artifact.content_type)
    )
}

/// 执行 `run_report_artifact_json` 对应的受控流程。
fn run_report_artifact_json(raw: &RawSkillRunReport) -> String {
    format!(
        "{{\"runner\":{},\"status\":{},\"exit_code\":{},\"duration_ms\":{},\"stdout\":{},\"stderr\":{},\"audit\":{}}}",
        json_string(&raw.runner),
        json_string(raw.status.as_str()),
        raw.exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "null".to_owned()),
        raw.duration_ms,
        provider_stream_summary_json(&raw.stdout_stream),
        provider_stream_summary_json(&raw.stderr_stream),
        json_array(raw.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 执行 `json_array` 对应的处理逻辑。
fn json_array(values: impl IntoIterator<Item = String>) -> String {
    format!("[{}]", values.into_iter().collect::<Vec<_>>().join(","))
}

/// 执行 `json_string` 对应的处理逻辑。
fn json_string(value: &str) -> String {
    let mut escaped = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            value => escaped.push(value),
        }
    }
    escaped.push('"');
    escaped
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{SkillInputProperty, SkillInputSchema};
    use crate::supervisor::{ProviderCredentialScope, PROVIDER_SESSION_TOKEN_ENV};
    use eva_config::AdapterTransport;
    use eva_core::{CapabilityName, ErrorKind};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 验证 `skill_schema_requires_declared_fields` 场景下的预期行为。
    #[test]
    fn skill_schema_requires_declared_fields() {
        let root = test_root("schema-required");
        let handle = skill_handle(None, Vec::new(), root.path.clone());

        let error = validate_skill_input(&handle, "{\"severity\":\"major\"}").unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

    /// 验证 `skill_schema_rejects_enum_values_before_runner_start` 场景下的预期行为。
    #[test]
    fn skill_schema_rejects_enum_values_before_runner_start() {
        let root = test_root("schema-enum");
        let handle = skill_handle(None, Vec::new(), root.path.clone());

        let error = validate_skill_input(&handle, "{\"scope\":\"outside\",\"severity\":\"major\"}")
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

    /// 验证 `builtin_codex_skill_writes_artifact_evidence` 场景下的预期行为。
    #[test]
    fn builtin_codex_skill_writes_artifact_evidence() {
        let root = test_root("builtin");
        let handle = skill_handle(None, Vec::new(), root.path.clone());

        let report = invoke(
            &handle,
            AdapterInvocation::new(
                RequestId::parse("req-skill-builtin").unwrap(),
                CapabilityName::parse("workflow.code_review").unwrap(),
            )
            .with_input("{\"scope\":\"current_diff\"}"),
        )
        .unwrap();

        assert_eq!(report.status, "completed");
        assert!(report.output.contains("\"runner\":\"builtin_codex_skill\""));
        assert!(report
            .output
            .contains("skill/code-review-skill/req-skill-builtin/stdout"));
        assert!(root.path.join("objects").exists());
    }

    /// 验证 `process_skill_runner_collects_artifacts_and_redacts_env` 场景下的预期行为。
    #[test]
    fn process_skill_runner_collects_artifacts_and_redacts_env() {
        let root = test_root("process");
        let env_name = "EVA_TEST_SKILL_SECRET";
        let secret = "skill-secret-redaction";
        let handle = skill_handle(
            Some(test_command().to_owned()),
            artifact_args(secret),
            root.path.clone(),
        )
        .with_credential_env(env_name);
        let request_id = RequestId::parse("req-skill-process").unwrap();
        let capability = CapabilityName::parse("workflow.code_review").unwrap();
        let scope = ProviderCredentialScope::new_for_session(
            "session-skill-process",
            handle.id.clone(),
            request_id.clone(),
            capability.clone(),
        );

        let vault =
            crate::credential_vault::MemoryCredentialVault::new().with_secret(env_name, secret);
        let report = invoke_with_spawner_and_vault(
            &handle,
            AdapterInvocation::new(request_id, capability)
                .with_credential_scope(scope)
                .with_input("{\"scope\":\"current_diff\",\"severity\":\"major\"}"),
            None,
            &vault,
        );
        let report = report.unwrap();

        assert_eq!(report.status, "completed");
        assert!(!report.output.contains(secret));
        assert!(!report.output.contains("eva-provider-session:"));
        assert!(report.output.contains("[REDACTED]"));
        assert!(report
            .output
            .contains("skill/code-review-skill/req-skill-process/artifacts/result.txt"));
        assert!(report
            .audit
            .contains(&format!("credential_env:{env_name}:redacted")));
        assert!(report
            .audit
            .contains(&"credential.session_token:redacted".to_owned()));
        let artifact = fs::read_to_string(root.path.join(
            "objects/skill/code-review-skill/req-skill-process/artifacts/result.txt.artifact",
        ))
        .unwrap();
        assert!(!artifact.contains(secret));
        assert!(!artifact.contains("eva-provider-session:"));
        assert!(artifact.contains("[REDACTED]"));
    }

    /// 验证 `process_skill_runner_records_failure_evidence` 场景下的预期行为。
    #[test]
    fn process_skill_runner_records_failure_evidence() {
        let root = test_root("failure");
        let handle = skill_handle(
            Some(test_command().to_owned()),
            fail_args(),
            root.path.clone(),
        );

        let report = invoke(
            &handle,
            AdapterInvocation::new(
                RequestId::parse("req-skill-failure").unwrap(),
                CapabilityName::parse("workflow.code_review").unwrap(),
            )
            .with_input("{\"scope\":\"current_diff\"}"),
        )
        .unwrap();

        assert_eq!(report.status, "failed");
        assert!(report.output.contains("\"stderr\":{\"stream\":\"stderr\""));
        assert!(report.output.contains("\"preview\":\"failure\""));
        assert!(report
            .output
            .contains("skill/code-review-skill/req-skill-failure/run-report"));
    }

    /// 验证 `process_skill_runner_reports_timeout` 场景下的预期行为。
    #[test]
    fn process_skill_runner_reports_timeout() {
        let root = test_root("timeout");
        let handle = skill_handle(
            Some(test_command().to_owned()),
            sleep_args(),
            root.path.clone(),
        )
        .with_timeout_ms(1);

        let report = invoke(
            &handle,
            AdapterInvocation::new(
                RequestId::parse("req-skill-timeout").unwrap(),
                CapabilityName::parse("workflow.code_review").unwrap(),
            )
            .with_input("{\"scope\":\"current_diff\"}"),
        )
        .unwrap();

        assert_eq!(report.status, "timeout");
        assert!(report.audit.contains(&"skill.status:timeout".to_owned()));
    }

    #[cfg(unix)]
    #[test]
    fn process_skill_runner_reaps_process_after_stdin_write_failure() {
        let root = test_root("stdin-write-failure");
        let work_root = root.path.join("work");
        let marker = work_root.join("artifacts").join("survived.txt");
        let config = SkillRunnerConfig::new(["sh"], 5_000, 4096, root.path.clone(), work_root);
        let invocation = SkillRunnerInvocation {
            adapter_id: AdapterId::parse("stdin-close-skill").unwrap(),
            request_id: RequestId::parse("req-stdin-close-skill").unwrap(),
            skill_id: "stdin-close".to_owned(),
            entry_type: "process".to_owned(),
            command: Some("sh".to_owned()),
            args: vec![
                "-c".to_owned(),
                "exec 0<&-; sleep 0.2; printf survived > \"$EVA_SKILL_ARTIFACT_DIR/survived.txt\""
                    .to_owned(),
            ],
            env: BTreeMap::new(),
            input: "x".repeat(1024 * 1024),
        };

        let error = SkillRunner.run(&config, invocation).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert_eq!(error.message(), "failed to write skill runner input");
        thread::sleep(Duration::from_millis(300));
        assert!(!marker.exists());
    }

    /// 验证 `artifact_collection_rejects_uncontrolled_relative_paths` 场景下的预期行为。
    #[test]
    fn artifact_collection_rejects_uncontrolled_relative_paths() {
        let root = test_root("bad-artifact");
        let handle = skill_handle(
            Some(test_command().to_owned()),
            bad_artifact_args(),
            root.path.clone(),
        );

        let error = invoke(
            &handle,
            AdapterInvocation::new(
                RequestId::parse("req-skill-bad-artifact").unwrap(),
                CapabilityName::parse("workflow.code_review").unwrap(),
            )
            .with_input("{\"scope\":\"current_diff\"}"),
        )
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

    #[cfg(unix)]
    #[test]
    fn skill_artifact_symlink_is_rejected_without_touching_target() {
        let root = test_root("artifact-symlink");
        let artifact_dir = root.path.join("artifacts");
        ensure_skill_directory_tree(&artifact_dir, "test artifact directory").unwrap();
        let target = root.path.join("outside.txt");
        fs::write(&target, b"outside-content").unwrap();
        let link = artifact_dir.join("result.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let mut store = FileSystemArtifactStore::new(root.path.join("store"));
        let mut artifacts = Vec::new();
        let error = collect_artifact_dir(
            &mut store,
            &artifact_dir,
            &artifact_dir,
            "skill/test",
            &[],
            &mut artifacts,
        )
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&target).unwrap(), b"outside-content");
    }

    #[cfg(unix)]
    #[test]
    fn skill_input_symlink_is_rejected_without_truncating_target() {
        let root = test_root("input-symlink");
        let work_dir = root.path.join("work");
        ensure_skill_directory_tree(&work_dir, "test work directory").unwrap();
        let target = root.path.join("outside-input.json");
        fs::write(&target, b"original-input").unwrap();
        let input_path = work_dir.join("input.json");
        std::os::unix::fs::symlink(&target, &input_path).unwrap();

        let error = write_skill_file_no_follow(
            &work_dir,
            &input_path,
            b"replacement-input",
            "test input evidence",
        )
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&target).unwrap(), b"original-input");
    }

    #[cfg(unix)]
    #[test]
    fn skill_artifact_ancestor_symlink_is_rejected() {
        let root = test_root("artifact-ancestor-symlink");
        let artifact_dir = root.path.join("artifacts");
        ensure_skill_directory_tree(&artifact_dir, "test artifact directory").unwrap();
        let nested = artifact_dir.join("nested");
        fs::create_dir(&nested).unwrap();
        let target_dir = root.path.join("outside-artifacts");
        fs::rename(&nested, &target_dir).unwrap();
        std::os::unix::fs::symlink(&target_dir, &nested).unwrap();
        fs::write(target_dir.join("result.txt"), b"outside-content").unwrap();

        let mut store = FileSystemArtifactStore::new(root.path.join("store"));
        let mut artifacts = Vec::new();
        let error = collect_artifact_dir(
            &mut store,
            &artifact_dir,
            &artifact_dir,
            "skill/test",
            &[],
            &mut artifacts,
        )
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(
            fs::read(target_dir.join("result.txt")).unwrap(),
            b"outside-content"
        );
    }

    /// 表示 `TestRoot` 数据结构。
    #[derive(Debug)]
    struct TestRoot {
        /// 记录 `path` 字段对应的值。
        path: PathBuf,
    }

    impl Drop for TestRoot {
        /// 停止或释放 `drop` 管理的资源。
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// 为测试提供可解引用为真实适配器句柄的轻量包装。
    #[derive(Debug, Clone)]
    struct TestSkillHandle(
        /// 保存被测技能运行时使用的底层适配器句柄。
        AdapterHandle,
    );

    impl TestSkillHandle {
        /// 设置 `credential_env` 并返回更新后的实例。
        fn with_credential_env(mut self, env_name: &str) -> AdapterHandle {
            self.0.credential_env = vec![env_name.to_owned()];
            self.0
        }

        /// 设置 `timeout_ms` 并返回更新后的实例。
        fn with_timeout_ms(mut self, timeout_ms: u64) -> AdapterHandle {
            self.0.timeout_ms = Some(timeout_ms);
            self.0
        }
    }

    impl std::ops::Deref for TestSkillHandle {
        /// 为 `Target` 定义类型别名。
        type Target = AdapterHandle;

        /// 执行 `deref` 对应的处理逻辑。
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    /// 执行 `skill_handle` 对应的处理逻辑。
    fn skill_handle(
        command: Option<String>,
        args: Vec<String>,
        artifact_root: PathBuf,
    ) -> TestSkillHandle {
        TestSkillHandle(AdapterHandle {
            id: AdapterId::parse("code-review-skill").unwrap(),
            name: "Code Review Skill".to_owned(),
            version: "1.0.0".to_owned(),
            enabled: true,
            transport: AdapterTransport::Skill,
            capabilities: vec![CapabilityName::parse("workflow.code_review").unwrap()],
            source_path: "test".to_owned(),
            command: None,
            args: Vec::new(),
            endpoint: None,
            method: None,
            credential_env: Vec::new(),
            provider: eva_config::ProviderConfig::default(),
            timeout_ms: Some(5_000),
            max_concurrency: None,
            output_limit_bytes: Some(4096),
            max_prompt_bytes: Some(4096),
            rate_limit: None,
            circuit_breaker: None,
            headers: BTreeMap::new(),
            mcp_server_transport: None,
            mcp_command: None,
            mcp_args: Vec::new(),
            mcp_tools: Vec::new(),
            mcp_http_config: None,
            mcp_http_config_invalid: false,
            skill_id: Some("code-review".to_owned()),
            skill_kind: Some("workflow_skill".to_owned()),
            skill_runtime_gate: Some("normal".to_owned()),
            skill_path: Some("test/SKILL.md".to_owned()),
            skill_entry_type: Some("codex_skill".to_owned()),
            skill_runner_command: command,
            skill_runner_args: args,
            skill_artifact_root: Some(artifact_root.display().to_string()),
            skill_input_schema: Some(SkillInputSchema {
                schema_type: Some("object".to_owned()),
                required: vec!["scope".to_owned()],
                properties: BTreeMap::from([
                    (
                        "scope".to_owned(),
                        SkillInputProperty {
                            value_type: Some("string".to_owned()),
                            enum_values: vec!["current_diff".to_owned(), "workspace".to_owned()],
                        },
                    ),
                    (
                        "severity".to_owned(),
                        SkillInputProperty {
                            value_type: Some("string".to_owned()),
                            enum_values: vec![
                                "all".to_owned(),
                                "major".to_owned(),
                                "critical".to_owned(),
                            ],
                        },
                    ),
                ]),
            }),
            hardware_logical_name: None,
            hardware_device_class: None,
            hardware_driver_id: None,
            hardware_driver_kind: None,
            bindings: Vec::new(),
        })
    }

    /// 执行 `test_root` 对应的处理逻辑。
    fn test_root(name: &str) -> TestRoot {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        TestRoot {
            path: env::temp_dir().join(format!(
                "eva-adapter-skill-{name}-{}-{now}",
                std::process::id()
            )),
        }
    }

    /// 执行 `test_command` 对应的处理逻辑。
    #[cfg(windows)]
    fn test_command() -> &'static str {
        "powershell"
    }

    /// 执行 `test_command` 对应的处理逻辑。
    #[cfg(not(windows))]
    fn test_command() -> &'static str {
        "sh"
    }

    /// 执行 `artifact_args` 对应的处理逻辑。
    #[cfg(windows)]
    fn artifact_args(secret: &str) -> Vec<String> {
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            format!(
                "$null=[Console]::In.ReadToEnd(); $dir=$env:EVA_SKILL_ARTIFACT_DIR; New-Item -ItemType Directory -Force -Path $dir | Out-Null; Set-Content -NoNewline -Path (Join-Path $dir 'result.txt') -Value ('artifact-' + '{secret}' + $env:{PROVIDER_SESSION_TOKEN_ENV}); [Console]::Out.Write('{secret}'); [Console]::Out.Write($env:{PROVIDER_SESSION_TOKEN_ENV}); [Console]::Error.Write('stderr-ok'); [Console]::Error.Write($env:{PROVIDER_SESSION_TOKEN_ENV})"
            ),
        ]
    }

    /// 执行 `artifact_args` 对应的处理逻辑。
    #[cfg(not(windows))]
    fn artifact_args(secret: &str) -> Vec<String> {
        vec![
            "-c".to_owned(),
            format!(
                "cat >/dev/null; mkdir -p \"$EVA_SKILL_ARTIFACT_DIR\"; printf 'artifact-{secret}' > \"$EVA_SKILL_ARTIFACT_DIR/result.txt\"; printf \"${PROVIDER_SESSION_TOKEN_ENV}\" >> \"$EVA_SKILL_ARTIFACT_DIR/result.txt\"; printf '{secret}'; printf \"${PROVIDER_SESSION_TOKEN_ENV}\"; printf stderr-ok >&2; printf \"${PROVIDER_SESSION_TOKEN_ENV}\" >&2"
            ),
        ]
    }

    /// 执行 `fail_args` 对应的处理逻辑。
    #[cfg(windows)]
    fn fail_args() -> Vec<String> {
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            "$null=[Console]::In.ReadToEnd(); [Console]::Error.Write('failure'); exit 7".to_owned(),
        ]
    }

    /// 执行 `fail_args` 对应的处理逻辑。
    #[cfg(not(windows))]
    fn fail_args() -> Vec<String> {
        vec![
            "-c".to_owned(),
            "cat >/dev/null; printf failure >&2; exit 7".to_owned(),
        ]
    }

    /// 执行 `sleep_args` 对应的处理逻辑。
    #[cfg(windows)]
    fn sleep_args() -> Vec<String> {
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            "$null=[Console]::In.ReadToEnd(); Start-Sleep -Milliseconds 200; [Console]::Out.Write('done')".to_owned(),
        ]
    }

    /// 执行 `sleep_args` 对应的处理逻辑。
    #[cfg(not(windows))]
    fn sleep_args() -> Vec<String> {
        vec![
            "-c".to_owned(),
            "cat >/dev/null; sleep 0.2; printf done".to_owned(),
        ]
    }

    /// 执行 `bad_artifact_args` 对应的处理逻辑。
    #[cfg(windows)]
    fn bad_artifact_args() -> Vec<String> {
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            "$null=[Console]::In.ReadToEnd(); $dir=$env:EVA_SKILL_ARTIFACT_DIR; New-Item -ItemType Directory -Force -Path $dir | Out-Null; Set-Content -NoNewline -Path (Join-Path $dir 'bad name.txt') -Value 'bad'".to_owned(),
        ]
    }

    /// 执行 `bad_artifact_args` 对应的处理逻辑。
    #[cfg(not(windows))]
    fn bad_artifact_args() -> Vec<String> {
        vec![
            "-c".to_owned(),
            "cat >/dev/null; mkdir -p \"$EVA_SKILL_ARTIFACT_DIR\"; printf bad > \"$EVA_SKILL_ARTIFACT_DIR/bad name.txt\"".to_owned(),
        ]
    }
}
