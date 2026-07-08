//! Workflow skill Adapter transport runner.

use crate::manifest::{AdapterHandle, SkillInputSchema};
use crate::runtime::{AdapterInvocation, AdapterInvokeReport};
use eva_core::{AdapterId, EvaError, RequestId};
use eva_storage::{ArtifactRecord, FileSystemArtifactStore};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "controlled workflow skill execution with schema gates and artifact evidence";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRunnerConfig {
    pub allowed_commands: BTreeSet<String>,
    pub timeout_ms: u64,
    pub output_limit_bytes: usize,
    pub artifact_root: PathBuf,
    pub work_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRunnerInvocation {
    pub adapter_id: AdapterId,
    pub request_id: RequestId,
    pub skill_id: String,
    pub entry_type: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub input: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRunReport {
    pub runner: String,
    pub status: SkillRunStatus,
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub duration_ms: u128,
    pub working_dir: PathBuf,
    pub artifact_root: PathBuf,
    pub artifacts: Vec<SkillArtifactEvidence>,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillArtifactEvidence {
    pub key: String,
    pub digest: String,
    pub size_bytes: usize,
    pub content_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillRunStatus {
    Completed,
    Failed,
    Timeout,
    OutputLimitExceeded,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SkillRunner;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunPaths {
    working_dir: PathBuf,
    artifact_dir: PathBuf,
    artifact_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawSkillRunReport {
    runner: String,
    status: SkillRunStatus,
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    duration_ms: u128,
    audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CredentialEnvValues {
    values: BTreeMap<String, String>,
    audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedJsonValue {
    String(String),
    Other,
}

impl SkillRunnerConfig {
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
            artifact_root: artifact_root.into(),
            work_root: work_root.into(),
        }
    }
}

impl SkillRunStatus {
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
    pub fn run(
        &self,
        config: &SkillRunnerConfig,
        invocation: SkillRunnerInvocation,
    ) -> Result<SkillRunReport, EvaError> {
        validate_runner_config(config, &invocation)?;
        let paths = prepare_run_paths(config, &invocation)?;
        let sensitive_values = sensitive_values(invocation.env.values());
        let raw = if let Some(command) = &invocation.command {
            run_process(config, &paths, &invocation, command, &sensitive_values)?
        } else if invocation.entry_type == "codex_skill" {
            run_builtin_codex_skill(&paths, &invocation)?
        } else {
            return Err(EvaError::unsupported(
                "skill entry type requires an explicit runner command",
            )
            .with_context("entry_type", invocation.entry_type)
            .with_context("skill_id", invocation.skill_id));
        };
        let artifacts = persist_run_artifacts(&paths, &invocation, &raw)?;

        Ok(SkillRunReport {
            runner: raw.runner,
            status: raw.status,
            exit_code: raw.exit_code,
            stdout: raw.stdout,
            stderr: raw.stderr,
            duration_ms: raw.duration_ms,
            working_dir: paths.working_dir,
            artifact_root: paths.artifact_root,
            artifacts,
            audit: raw.audit,
        })
    }
}

pub fn invoke(
    handle: &AdapterHandle,
    invocation: AdapterInvocation,
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

    let trace = invocation.trace_for_adapter(&handle.id);
    let request_id = invocation.request_id;
    let capability = invocation.capability;
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
    let credential_env = credential_env_values(&handle.credential_env);
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
    let run = SkillRunner.run(
        &config,
        SkillRunnerInvocation {
            adapter_id: handle.id.clone(),
            request_id: request_id.clone(),
            skill_id: skill.to_owned(),
            entry_type,
            command,
            args,
            env: credential_env.values.clone(),
            input: invocation.input,
        },
    )?;

    let mut audit = vec![format!("adapter.invoked:{}", handle.id.as_str())];
    audit.extend(run.audit.clone());
    audit.extend(credential_env.audit);
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

fn prepare_run_paths(
    config: &SkillRunnerConfig,
    invocation: &SkillRunnerInvocation,
) -> Result<RunPaths, EvaError> {
    let working_dir = config.work_root.clone();
    let artifact_dir = working_dir.join("artifacts");
    fs::create_dir_all(&artifact_dir).map_err(|error| {
        EvaError::internal("failed to create skill working directory")
            .with_context("adapter_id", invocation.adapter_id.as_str())
            .with_context("path", artifact_dir.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    let input_path = working_dir.join("input.json");
    fs::write(&input_path, invocation.input.as_bytes()).map_err(|error| {
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

fn run_builtin_codex_skill(
    paths: &RunPaths,
    invocation: &SkillRunnerInvocation,
) -> Result<RawSkillRunReport, EvaError> {
    let started_at = Instant::now();
    let result = format!(
        "{{\"summary\":\"controlled workflow skill completed\",\"findings\":[],\"skill_id\":{},\"input\":{}}}",
        json_string(&invocation.skill_id),
        invocation.input
    );
    let result_path = paths.artifact_dir.join("result.json");
    fs::write(&result_path, result.as_bytes()).map_err(|error| {
        EvaError::internal("failed to write built-in skill result artifact")
            .with_context("skill_id", &invocation.skill_id)
            .with_context("path", result_path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    Ok(RawSkillRunReport {
        runner: "builtin_codex_skill".to_owned(),
        status: SkillRunStatus::Completed,
        exit_code: Some(0),
        stdout: result.into_bytes(),
        stderr: Vec::new(),
        duration_ms: started_at.elapsed().as_millis(),
        audit: vec![
            "transport:skill".to_owned(),
            "skill.runner:builtin_codex_skill".to_owned(),
            "skill.working_dir:isolated".to_owned(),
            "skill.artifact_dir:controlled".to_owned(),
        ],
    })
}

fn run_process(
    config: &SkillRunnerConfig,
    paths: &RunPaths,
    invocation: &SkillRunnerInvocation,
    command: &str,
    sensitive_values: &[String],
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
    let mut child = Command::new(command)
        .args(&invocation.args)
        .envs(env_values)
        .current_dir(&paths.working_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            EvaError::unavailable("failed to start skill runner process")
                .with_context("command", command)
                .with_context("io_error", error.to_string())
        })?;

    if !invocation.input.is_empty() {
        let mut stdin = child.stdin.take().ok_or_else(|| {
            EvaError::internal("skill runner stdin was not available")
                .with_context("command", command)
        })?;
        stdin
            .write_all(invocation.input.as_bytes())
            .map_err(|error| {
                EvaError::unavailable("failed to write skill runner input")
                    .with_context("command", command)
                    .with_context("io_error", error.to_string())
            })?;
    }
    drop(child.stdin.take());

    let stdout = child.stdout.take().ok_or_else(|| {
        EvaError::internal("skill runner stdout was not available").with_context("command", command)
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        EvaError::internal("skill runner stderr was not available").with_context("command", command)
    })?;
    let (sender, receiver) = mpsc::channel();
    spawn_reader("stdout", stdout, config.output_limit_bytes, sender.clone());
    spawn_reader("stderr", stderr, config.output_limit_bytes, sender);

    let timeout = Duration::from_millis(config.timeout_ms);
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let mut received_streams = 0;
    while received_streams < 2 {
        let message = if timeout.is_zero() {
            receiver.recv().map_err(mpsc::RecvTimeoutError::from)
        } else {
            let elapsed = started_at.elapsed();
            if elapsed >= timeout {
                kill_child(&mut child);
                return Ok(raw_process_report(
                    command,
                    SkillRunStatus::Timeout,
                    None,
                    stdout_bytes,
                    stderr_bytes,
                    started_at,
                    sensitive_values,
                ));
            }
            receiver.recv_timeout(timeout - elapsed)
        };
        match message {
            Ok(ReaderMessage::Output { stream, bytes }) => {
                received_streams += 1;
                if stream == "stdout" {
                    stdout_bytes = bytes;
                } else {
                    stderr_bytes = bytes;
                }
            }
            Ok(ReaderMessage::LimitExceeded { stream, bytes }) => {
                kill_child(&mut child);
                if stream == "stdout" {
                    stdout_bytes = bytes;
                } else {
                    stderr_bytes = bytes;
                }
                return Ok(raw_process_report(
                    command,
                    SkillRunStatus::OutputLimitExceeded,
                    None,
                    stdout_bytes,
                    stderr_bytes,
                    started_at,
                    sensitive_values,
                ));
            }
            Ok(ReaderMessage::ReadError { stream, error }) => {
                kill_child(&mut child);
                return Err(EvaError::unavailable("failed to read skill runner output")
                    .with_context("command", command)
                    .with_context("stream", stream)
                    .with_context("io_error", error));
            }
            Err(_) => {
                kill_child(&mut child);
                return Ok(raw_process_report(
                    command,
                    SkillRunStatus::Timeout,
                    None,
                    stdout_bytes,
                    stderr_bytes,
                    started_at,
                    sensitive_values,
                ));
            }
        }
    }

    loop {
        if let Some(status) = child.try_wait().map_err(|error| {
            EvaError::unavailable("failed to read skill runner process status")
                .with_context("command", command)
                .with_context("io_error", error.to_string())
        })? {
            let exit_code = status.code();
            let run_status = if exit_code == Some(0) {
                SkillRunStatus::Completed
            } else {
                SkillRunStatus::Failed
            };
            return Ok(raw_process_report(
                command,
                run_status,
                exit_code,
                stdout_bytes,
                stderr_bytes,
                started_at,
                sensitive_values,
            ));
        }
        if !timeout.is_zero() && started_at.elapsed() >= timeout {
            kill_child(&mut child);
            return Ok(raw_process_report(
                command,
                SkillRunStatus::Timeout,
                None,
                stdout_bytes,
                stderr_bytes,
                started_at,
                sensitive_values,
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn raw_process_report(
    command: &str,
    status: SkillRunStatus,
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    started_at: Instant,
    sensitive_values: &[String],
) -> RawSkillRunReport {
    RawSkillRunReport {
        runner: "process".to_owned(),
        status,
        exit_code,
        stdout: redact_bytes(stdout, sensitive_values),
        stderr: redact_bytes(stderr, sensitive_values),
        duration_ms: started_at.elapsed().as_millis(),
        audit: vec![
            "transport:skill".to_owned(),
            "skill.runner:process".to_owned(),
            "shell:false".to_owned(),
            format!("skill.command:{command}"),
        ],
    }
}

fn persist_run_artifacts(
    paths: &RunPaths,
    invocation: &SkillRunnerInvocation,
    raw: &RawSkillRunReport,
) -> Result<Vec<SkillArtifactEvidence>, EvaError> {
    let mut store = FileSystemArtifactStore::new(&paths.artifact_root);
    let base_key = format!(
        "skill/{}/{}",
        safe_segment(invocation.adapter_id.as_str()),
        safe_segment(invocation.request_id.as_str())
    );
    let mut artifacts = Vec::new();
    artifacts.push(evidence_from_record(store.put_bytes_with_metadata(
        format!("{base_key}/stdout"),
        raw.stdout.clone(),
        "text/plain",
        "retain",
        None,
    )?));
    artifacts.push(evidence_from_record(store.put_bytes_with_metadata(
        format!("{base_key}/stderr"),
        raw.stderr.clone(),
        "text/plain",
        "retain",
        None,
    )?));
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
            &mut artifacts,
        )?;
    }
    Ok(artifacts)
}

fn collect_artifact_dir(
    store: &mut FileSystemArtifactStore,
    root_dir: &Path,
    artifact_dir: &Path,
    base_key: &str,
    artifacts: &mut Vec<SkillArtifactEvidence>,
) -> Result<(), EvaError> {
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
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            EvaError::internal("failed to read skill artifact metadata")
                .with_context("path", path.display().to_string())
                .with_context("io_error", error.to_string())
        })?;
        if metadata.file_type().is_symlink() {
            return Err(
                EvaError::permission_denied("skill artifact symlink is not allowed")
                    .with_context("path", path.display().to_string()),
            );
        }
        if metadata.is_dir() {
            collect_artifact_dir(store, root_dir, &path, base_key, artifacts)?;
        } else if metadata.is_file() {
            let relative = path.strip_prefix(root_dir).map_err(|error| {
                EvaError::internal("failed to compute skill artifact relative path")
                    .with_context("path", path.display().to_string())
                    .with_context("path_error", error.to_string())
            })?;
            let relative_key = relative_artifact_key(relative)?;
            let bytes = fs::read(&path).map_err(|error| {
                EvaError::internal("failed to read skill artifact")
                    .with_context("path", path.display().to_string())
                    .with_context("io_error", error.to_string())
            })?;
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

fn evidence_from_record(record: ArtifactRecord) -> SkillArtifactEvidence {
    SkillArtifactEvidence {
        key: record.key,
        digest: record.digest,
        size_bytes: record.size_bytes,
        content_type: record.content_type,
    }
}

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

fn parse_json_object(input: &str) -> Result<BTreeMap<String, ParsedJsonValue>, EvaError> {
    let mut parser = JsonObjectParser::new(input);
    parser.parse_object()
}

struct JsonObjectParser<'a> {
    input: &'a str,
    chars: Vec<char>,
    index: usize,
}

impl<'a> JsonObjectParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            chars: input.chars().collect(),
            index: 0,
        }
    }

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

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(value) if value.is_whitespace()) {
            self.index += 1;
        }
    }

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

    fn consume(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).copied()
    }

    fn next(&mut self) -> Option<char> {
        let value = self.peek()?;
        self.index += 1;
        Some(value)
    }

    fn byte_index(&self) -> usize {
        self.chars
            .iter()
            .take(self.index)
            .map(|character| character.len_utf8())
            .sum()
    }
}

enum ReaderMessage {
    Output {
        stream: &'static str,
        bytes: Vec<u8>,
    },
    LimitExceeded {
        stream: &'static str,
        bytes: Vec<u8>,
    },
    ReadError {
        stream: &'static str,
        error: String,
    },
}

fn spawn_reader(
    stream: &'static str,
    reader: impl Read + Send + 'static,
    limit: usize,
    sender: mpsc::Sender<ReaderMessage>,
) {
    thread::spawn(move || {
        let mut output = Vec::new();
        let mut reader = BufReader::new(reader);
        loop {
            let buffer = match reader.fill_buf() {
                Ok(buffer) => buffer,
                Err(error) => {
                    let _ = sender.send(ReaderMessage::ReadError {
                        stream,
                        error: error.to_string(),
                    });
                    return;
                }
            };
            if buffer.is_empty() {
                let _ = sender.send(ReaderMessage::Output {
                    stream,
                    bytes: output,
                });
                return;
            }
            let remaining = limit.saturating_sub(output.len());
            if buffer.len() > remaining {
                output.extend_from_slice(&buffer[..remaining]);
                let _ = sender.send(ReaderMessage::LimitExceeded {
                    stream,
                    bytes: output,
                });
                return;
            }
            let consumed = buffer.len();
            output.extend_from_slice(buffer);
            reader.consume(consumed);
        }
    });
}

fn kill_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn credential_env_values(names: &[String]) -> CredentialEnvValues {
    let mut values = BTreeMap::new();
    let mut audit = Vec::new();
    for name in names {
        match env::var(name) {
            Ok(value) => {
                values.insert(name.clone(), value);
                audit.push(format!("credential_env:{name}:redacted"));
            }
            Err(_) => audit.push(format!("credential_env:{name}:missing")),
        }
    }
    CredentialEnvValues { values, audit }
}

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

fn timeout_ms(handle: &AdapterHandle) -> u64 {
    handle.timeout_ms.unwrap_or(30_000)
}

fn output_limit_bytes(handle: &AdapterHandle) -> usize {
    handle
        .output_limit_bytes
        .or(handle.max_prompt_bytes)
        .unwrap_or(64 * 1024)
}

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

fn project_root_from_manifest_path(path: &Path) -> Option<PathBuf> {
    let config_dir = path.parent()?.parent()?;
    if config_dir.file_name().and_then(|value| value.to_str()) == Some("config") {
        return config_dir.parent().map(Path::to_path_buf);
    }
    None
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

fn sensitive_values<'a>(values: impl IntoIterator<Item = &'a String>) -> Vec<String> {
    values
        .into_iter()
        .filter(|value| !value.is_empty())
        .cloned()
        .collect()
}

fn redact_bytes(bytes: Vec<u8>, sensitive_values: &[String]) -> Vec<u8> {
    if sensitive_values.is_empty() {
        return bytes;
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    for value in sensitive_values {
        text = text.replace(value, "[REDACTED]");
    }
    text.into_bytes()
}

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

fn skill_output_json(skill: &str, run: &SkillRunReport) -> String {
    format!(
        "{{\"transport\":\"skill\",\"skill\":{},\"runner\":{},\"status\":{},\"exit_code\":{},\"stdout\":{},\"stderr\":{},\"duration_ms\":{},\"working_dir\":{},\"artifact_root\":{},\"artifacts\":{}}}",
        json_string(skill),
        json_string(&run.runner),
        json_string(run.status.as_str()),
        run.exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "null".to_owned()),
        json_string(&String::from_utf8_lossy(&run.stdout)),
        json_string(&String::from_utf8_lossy(&run.stderr)),
        run.duration_ms,
        json_string(&run.working_dir.display().to_string()),
        json_string(&run.artifact_root.display().to_string()),
        json_array(run.artifacts.iter().map(artifact_json))
    )
}

fn artifact_json(artifact: &SkillArtifactEvidence) -> String {
    format!(
        "{{\"key\":{},\"digest\":{},\"size_bytes\":{},\"content_type\":{}}}",
        json_string(&artifact.key),
        json_string(&artifact.digest),
        artifact.size_bytes,
        json_string(&artifact.content_type)
    )
}

fn run_report_artifact_json(raw: &RawSkillRunReport) -> String {
    format!(
        "{{\"runner\":{},\"status\":{},\"exit_code\":{},\"duration_ms\":{},\"audit\":{}}}",
        json_string(&raw.runner),
        json_string(raw.status.as_str()),
        raw.exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "null".to_owned()),
        raw.duration_ms,
        json_array(raw.audit.iter().map(|entry| json_string(entry)))
    )
}

fn json_array(values: impl IntoIterator<Item = String>) -> String {
    format!("[{}]", values.into_iter().collect::<Vec<_>>().join(","))
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{SkillInputProperty, SkillInputSchema};
    use eva_config::AdapterTransport;
    use eva_core::{CapabilityName, ErrorKind};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn skill_schema_requires_declared_fields() {
        let root = test_root("schema-required");
        let handle = skill_handle(None, Vec::new(), root.path.clone());

        let error = validate_skill_input(&handle, "{\"severity\":\"major\"}").unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

    #[test]
    fn skill_schema_rejects_enum_values_before_runner_start() {
        let root = test_root("schema-enum");
        let handle = skill_handle(None, Vec::new(), root.path.clone());

        let error = validate_skill_input(&handle, "{\"scope\":\"outside\",\"severity\":\"major\"}")
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

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

    #[test]
    fn process_skill_runner_collects_artifacts_and_redacts_env() {
        let root = test_root("process");
        let env_name = "EVA_TEST_SKILL_SECRET";
        let secret = "skill-secret-redaction";
        env::set_var(env_name, secret);
        let handle = skill_handle(
            Some(test_command().to_owned()),
            artifact_args(secret),
            root.path.clone(),
        )
        .with_credential_env(env_name);

        let report = invoke(
            &handle,
            AdapterInvocation::new(
                RequestId::parse("req-skill-process").unwrap(),
                CapabilityName::parse("workflow.code_review").unwrap(),
            )
            .with_input("{\"scope\":\"current_diff\",\"severity\":\"major\"}"),
        )
        .unwrap();
        env::remove_var(env_name);

        assert_eq!(report.status, "completed");
        assert!(!report.output.contains(secret));
        assert!(report.output.contains("[REDACTED]"));
        assert!(report
            .output
            .contains("skill/code-review-skill/req-skill-process/artifacts/result.txt"));
        assert!(report
            .audit
            .contains(&format!("credential_env:{env_name}:redacted")));
    }

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
        assert!(report.output.contains("\"stderr\":\"failure\""));
        assert!(report
            .output
            .contains("skill/code-review-skill/req-skill-failure/run-report"));
    }

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

    #[derive(Debug)]
    struct TestRoot {
        path: PathBuf,
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[derive(Debug, Clone)]
    struct TestSkillHandle(AdapterHandle);

    impl TestSkillHandle {
        fn with_credential_env(mut self, env_name: &str) -> AdapterHandle {
            self.0.credential_env = vec![env_name.to_owned()];
            self.0
        }

        fn with_timeout_ms(mut self, timeout_ms: u64) -> AdapterHandle {
            self.0.timeout_ms = Some(timeout_ms);
            self.0
        }
    }

    impl std::ops::Deref for TestSkillHandle {
        type Target = AdapterHandle;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

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
            timeout_ms: Some(5_000),
            output_limit_bytes: Some(4096),
            max_prompt_bytes: Some(4096),
            headers: BTreeMap::new(),
            mcp_server_transport: None,
            mcp_command: None,
            mcp_args: Vec::new(),
            mcp_tools: Vec::new(),
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

    #[cfg(windows)]
    fn test_command() -> &'static str {
        "powershell"
    }

    #[cfg(not(windows))]
    fn test_command() -> &'static str {
        "sh"
    }

    #[cfg(windows)]
    fn artifact_args(secret: &str) -> Vec<String> {
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            format!(
                "$dir=$env:EVA_SKILL_ARTIFACT_DIR; New-Item -ItemType Directory -Force -Path $dir | Out-Null; Set-Content -NoNewline -Path (Join-Path $dir 'result.txt') -Value 'artifact-ok'; [Console]::Out.Write('{secret}'); [Console]::Error.Write('stderr-ok')"
            ),
        ]
    }

    #[cfg(not(windows))]
    fn artifact_args(secret: &str) -> Vec<String> {
        vec![
            "-c".to_owned(),
            format!(
                "mkdir -p \"$EVA_SKILL_ARTIFACT_DIR\"; printf artifact-ok > \"$EVA_SKILL_ARTIFACT_DIR/result.txt\"; printf '{secret}'; printf stderr-ok >&2"
            ),
        ]
    }

    #[cfg(windows)]
    fn fail_args() -> Vec<String> {
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            "[Console]::Error.Write('failure'); exit 7".to_owned(),
        ]
    }

    #[cfg(not(windows))]
    fn fail_args() -> Vec<String> {
        vec!["-c".to_owned(), "printf failure >&2; exit 7".to_owned()]
    }

    #[cfg(windows)]
    fn sleep_args() -> Vec<String> {
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            "Start-Sleep -Milliseconds 200; [Console]::Out.Write('done')".to_owned(),
        ]
    }

    #[cfg(not(windows))]
    fn sleep_args() -> Vec<String> {
        vec!["-c".to_owned(), "sleep 0.2; printf done".to_owned()]
    }

    #[cfg(windows)]
    fn bad_artifact_args() -> Vec<String> {
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            "$dir=$env:EVA_SKILL_ARTIFACT_DIR; New-Item -ItemType Directory -Force -Path $dir | Out-Null; Set-Content -NoNewline -Path (Join-Path $dir 'bad name.txt') -Value 'bad'".to_owned(),
        ]
    }

    #[cfg(not(windows))]
    fn bad_artifact_args() -> Vec<String> {
        vec![
            "-c".to_owned(),
            "mkdir -p \"$EVA_SKILL_ARTIFACT_DIR\"; printf bad > \"$EVA_SKILL_ARTIFACT_DIR/bad name.txt\"".to_owned(),
        ]
    }
}
