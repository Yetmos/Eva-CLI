//! 在不经过 shell 的前提下启动命令白名单中的 stdio 提供者。
//!
//! 标准输出和标准错误由独立读取线程并行采集，主线程以同一截止时间等待两路结果；超时、
//! 读取失败或任一路超过输出上限时都会终止并回收子进程。凭据仅从允许的环境变量和本次
//! 会话作用域注入，采集结果与审计内容在返回前完成脱敏。
//! Stdio transport runner contract.

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "stdio command transport with separated command and args";

use crate::manifest::AdapterHandle;
use crate::process_backend::{OsProcessBackend, ProviderProcessHandle, ProviderProcessSpawner};
use crate::runtime::{AdapterInvocation as RuntimeAdapterInvocation, AdapterInvokeReport};
use crate::stream::{
    collect_provider_stream, default_provider_artifact_root, provider_stream_audit,
    provider_stream_key, provider_stream_summary_json, ProviderStreamCapture, ProviderStreamConfig,
    DEFAULT_STREAM_CHUNK_SIZE_BYTES, DEFAULT_STREAM_PREVIEW_LIMIT_BYTES,
};
use crate::supervisor::validate_credential_scope_for_provider;
use eva_config::ProviderRunAsIdentity;
use eva_core::EvaError;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// 定义 stdio 子进程启动和两路输出采集的硬边界。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdioRunnerConfig {
    /// 保存允许直接传给 `Command::new` 的完整命令文本，不做 shell 展开。
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
    pub artifact_root: Option<PathBuf>,
    /// 记录 `artifact_key_prefix` 字段对应的值。
    pub artifact_key_prefix: Option<String>,
}

/// 表示 `StdioInvocation` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdioInvocation {
    /// 记录 `command` 字段对应的值。
    pub command: String,
    /// 记录 `args` 字段对应的值。
    pub args: Vec<String>,
    /// 记录 `env` 字段对应的值。
    pub env: BTreeMap<String, String>,
    /// 记录 `input` 字段对应的值。
    pub input: Vec<u8>,
}

/// 汇总子进程终态及 stdout、stderr 两路独立的有界采集证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdioRunReport {
    /// 记录 `command` 字段对应的值。
    pub command: String,
    /// 记录 `args` 字段对应的值。
    pub args: Vec<String>,
    /// OS PID captured at spawn time; the durable supervisor record carries
    /// the matching start token and group/job identity.
    pub process_id: u32,
    /// 记录 `status` 字段对应的值。
    pub status: StdioRunStatus,
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
    /// 记录 `audit` 字段对应的值。
    pub audit: Vec<String>,
}

/// 定义 `StdioRunStatus` 可取的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioRunStatus {
    /// 表示 `Completed` 枚举分支。
    Completed,
    /// 表示 `OutputLimitExceeded` 枚举分支。
    OutputLimitExceeded,
}

/// 无状态的 stdio 运行器；每次调用独占其子进程和读取线程。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StdioRunner;

impl StdioRunnerConfig {
    /// 创建并初始化当前类型的实例。
    pub fn new(
        allowed_commands: impl IntoIterator<Item = impl Into<String>>,
        timeout_ms: u64,
        output_limit_bytes: usize,
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
            artifact_root: None,
            artifact_key_prefix: None,
        }
    }

    /// 设置 `artifact_sink` 并返回更新后的实例。
    pub fn with_artifact_sink(
        mut self,
        artifact_root: impl Into<PathBuf>,
        artifact_key_prefix: impl Into<String>,
    ) -> Self {
        self.artifact_root = Some(artifact_root.into());
        self.artifact_key_prefix = Some(artifact_key_prefix.into());
        self
    }
}

impl StdioInvocation {
    /// 创建并初始化当前类型的实例。
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            input: Vec::new(),
        }
    }

    /// 设置 `args` 并返回更新后的实例。
    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// 设置 `env` 并返回更新后的实例。
    pub fn with_env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env = env;
        self
    }

    /// 设置 `input` 并返回更新后的实例。
    pub fn with_input(mut self, input: impl Into<Vec<u8>>) -> Self {
        self.input = input.into();
        self
    }
}

impl StdioRunStatus {
    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::OutputLimitExceeded => "output_limit_exceeded",
        }
    }
}

impl StdioRunner {
    /// Validate limits, spawn through the default OS backend, and reap the
    /// complete provider boundary on every error path.
    pub fn run(
        &self,
        config: &StdioRunnerConfig,
        invocation: StdioInvocation,
    ) -> Result<StdioRunReport, EvaError> {
        let backend = OsProcessBackend::new();
        self.run_with_spawner_as(
            config,
            invocation,
            &backend,
            &ProviderRunAsIdentity::Current,
        )
    }

    /// Run with an injected process spawner. Runtime callers pass a wrapper
    /// that performs durable process-record registration before returning the
    /// handle; a registration error therefore drops/terminates the handle.
    pub fn run_with_spawner<S: ProviderProcessSpawner + ?Sized>(
        &self,
        config: &StdioRunnerConfig,
        invocation: StdioInvocation,
        spawner: &S,
    ) -> Result<StdioRunReport, EvaError> {
        self.run_with_spawner_as(config, invocation, spawner, &ProviderRunAsIdentity::Current)
    }

    /// Run with an injected process spawner and an explicit provider identity.
    /// The high-level Adapter path uses this method so a manifest identity
    /// cannot be silently replaced with the daemon identity.
    pub fn run_with_spawner_as<S: ProviderProcessSpawner + ?Sized>(
        &self,
        config: &StdioRunnerConfig,
        invocation: StdioInvocation,
        spawner: &S,
        run_as: &ProviderRunAsIdentity,
    ) -> Result<StdioRunReport, EvaError> {
        spawner.validate_provider_run_as(run_as)?;
        validate_invocation(config, &invocation)?;

        let started_at = Instant::now();
        let sensitive_values = sensitive_values(invocation.env.values());
        // 命令与参数分别传递，不拼接 shell 字符串，从边界上禁止 shell 注入语义。
        let mut command = Command::new(&invocation.command);
        command
            .args(&invocation.args)
            .envs(&invocation.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = spawner
            .spawn_provider_as(command, run_as)
            .map_err(|error| {
                if error.message() == "failed to spawn provider process boundary" {
                    let mut mapped = EvaError::new(error.kind(), "failed to start stdio provider")
                        .with_retryable(error.is_retryable())
                        .with_error_context(error.context().clone());
                    if let Some(code) = error.provider_code() {
                        mapped = mapped.with_provider_code(code.as_str());
                    }
                    mapped.with_context("command", &invocation.command)
                } else {
                    error.with_context("command", &invocation.command)
                }
            })?;
        let process_id = child.pid();

        if !invocation.input.is_empty() {
            let Some(mut stdin) = child.take_stdin() else {
                terminate_process(&mut child);
                return Err(EvaError::internal("stdio provider stdin was not available")
                    .with_context("command", &invocation.command));
            };
            if let Err(error) = stdin.write_all(&invocation.input) {
                drop(stdin);
                terminate_process(&mut child);
                return Err(
                    EvaError::unavailable("failed to write stdio provider input")
                        .with_context("command", &invocation.command)
                        .with_context("io_error", error.to_string()),
                );
            }
        }
        drop(child.take_stdin());

        let Some(stdout) = child.take_stdout() else {
            terminate_process(&mut child);
            return Err(
                EvaError::internal("stdio provider stdout was not available")
                    .with_context("command", &invocation.command),
            );
        };
        let Some(stderr) = child.take_stderr() else {
            terminate_process(&mut child);
            return Err(
                EvaError::internal("stdio provider stderr was not available")
                    .with_context("command", &invocation.command),
            );
        };
        // 两路管道必须并发排空，避免子进程因任一路缓冲区写满而与父进程互相等待。
        let (sender, receiver) = mpsc::channel();
        spawn_reader(
            stream_config(config, "stdout"),
            stdout,
            sensitive_values.clone(),
            sender.clone(),
        );
        spawn_reader(
            stream_config(config, "stderr"),
            stderr,
            sensitive_values.clone(),
            sender,
        );

        let timeout = Duration::from_millis(config.timeout_ms);
        let output_deadline = if timeout.is_zero() {
            None
        } else {
            Some(started_at + timeout)
        };
        let mut stdout_capture = ProviderStreamCapture::empty("stdout");
        let mut stderr_capture = ProviderStreamCapture::empty("stderr");
        for _ in 0..2 {
            let message = match output_deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        terminate_process(&mut child);
                        return Err(EvaError::timeout("stdio provider timed out")
                            .with_context("command", &invocation.command)
                            .with_context("timeout_ms", config.timeout_ms.to_string()));
                    }
                    receiver.recv_timeout(deadline.saturating_duration_since(now))
                }
                None => receiver.recv().map_err(mpsc::RecvTimeoutError::from),
            }
            .map_err(|_| {
                terminate_process(&mut child);
                EvaError::timeout("stdio provider timed out")
                    .with_context("command", &invocation.command)
                    .with_context("timeout_ms", config.timeout_ms.to_string())
            })?;

            match message {
                ReaderMessage::Output { capture } => {
                    let truncated = capture.truncated;
                    let stream = capture.stream_name.clone();
                    if stream == "stdout" {
                        stdout_capture = capture;
                    } else {
                        stderr_capture = capture;
                    }
                    if truncated {
                        // 任一路触限都终止整个子进程；另一流可能只包含触限前已收到的证据。
                        terminate_process(&mut child);
                        let mut audit =
                            stdio_audit(config, &stdout_capture, &stderr_capture, Some(&stream));
                        audit.push(format!("process_id:{process_id}"));
                        return Ok(StdioRunReport {
                            command: invocation.command,
                            args: invocation.args,
                            process_id,
                            status: StdioRunStatus::OutputLimitExceeded,
                            exit_code: None,
                            stdout: stdout_capture.preview.clone(),
                            stderr: stderr_capture.preview.clone(),
                            stdout_stream: stdout_capture,
                            stderr_stream: stderr_capture,
                            duration_ms: started_at.elapsed().as_millis(),
                            audit,
                        });
                    }
                }
                ReaderMessage::ReadError { stream, error } => {
                    terminate_process(&mut child);
                    return Err(
                        EvaError::unavailable("failed to read stdio provider output")
                            .with_context("command", &invocation.command)
                            .with_context("stream", stream)
                            .with_context("io_error", error),
                    );
                }
            }
        }

        loop {
            if let Some(status) = child.try_wait().map_err(|error| {
                EvaError::unavailable("failed to read stdio provider status")
                    .with_context("command", &invocation.command)
                    .with_context("io_error", error.to_string())
            })? {
                return Ok(StdioRunReport {
                    command: invocation.command,
                    args: invocation.args,
                    process_id,
                    status: StdioRunStatus::Completed,
                    exit_code: status.code(),
                    stdout: stdout_capture.preview.clone(),
                    stderr: stderr_capture.preview.clone(),
                    stdout_stream: stdout_capture.clone(),
                    stderr_stream: stderr_capture.clone(),
                    duration_ms: started_at.elapsed().as_millis(),
                    audit: {
                        let mut audit = vec![
                            "transport:stdio".to_owned(),
                            "shell:false".to_owned(),
                            "stdio:completed".to_owned(),
                            format!("process_id:{process_id}"),
                        ];
                        audit.extend(provider_stream_audit(&stdout_capture));
                        audit.extend(provider_stream_audit(&stderr_capture));
                        audit
                    },
                });
            }
            if !timeout.is_zero() && started_at.elapsed() >= timeout {
                terminate_process(&mut child);
                return Err(EvaError::timeout("stdio provider timed out")
                    .with_context("command", &invocation.command)
                    .with_context("timeout_ms", config.timeout_ms.to_string()));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

/// 将已授权适配器调用转换为 stdio 运行器配置，并仅注入本次会话范围内的凭据。
pub fn invoke(
    handle: &AdapterHandle,
    invocation: RuntimeAdapterInvocation,
) -> Result<AdapterInvokeReport, EvaError> {
    invoke_with_spawner(handle, invocation, None)
}

/// Invoke stdio with an optional central process registrar. The supervised
/// runtime supplies one; standalone callers retain the default backend.
pub fn invoke_with_spawner(
    handle: &AdapterHandle,
    invocation: RuntimeAdapterInvocation,
    process_spawner: Option<&dyn ProviderProcessSpawner>,
) -> Result<AdapterInvokeReport, EvaError> {
    let command = handle.command.as_deref().ok_or_else(|| {
        EvaError::invalid_argument("stdio adapter is missing command")
            .with_context("adapter_id", handle.id.as_str())
    })?;
    validate_input_size(handle, &invocation.input)?;

    // Identity admission must precede credential-session validation and env
    // reads. Direct transport callers do not pass through AdapterRuntime's
    // earlier shape check, so keep this boundary local as well.
    match process_spawner {
        Some(spawner) => spawner.validate_provider_run_as(&handle.provider.run_as)?,
        None => OsProcessBackend::new().validate_run_as(&handle.provider.run_as)?,
    }

    let trace = invocation.trace_for_adapter(&handle.id);
    let credential_scope = validate_credential_scope_for_provider(
        invocation.credential_scope(),
        &handle.id,
        &invocation.request_id,
        &invocation.capability,
        !handle.credential_env.is_empty(),
    )?
    .cloned();
    let request_id = invocation.request_id;
    let capability = invocation.capability;
    let mut credential_env = credential_env_values(&handle.credential_env);
    if let Some(scope) = &credential_scope {
        scope.apply_env(&mut credential_env.values);
    }
    let artifact_root = default_provider_artifact_root(&handle.source_path);
    let artifact_key_prefix =
        provider_stream_key("provider", handle.id.as_str(), request_id.as_str(), "stdio");
    let config = StdioRunnerConfig::new(
        [command.to_owned()],
        timeout_ms(handle),
        output_limit_bytes(handle),
    )
    .with_artifact_sink(artifact_root, artifact_key_prefix);
    let stdio_invocation = StdioInvocation::new(command)
        .with_args(handle.args.clone())
        .with_env(credential_env.values.clone())
        .with_input(invocation.input.into_bytes());
    let run = match process_spawner {
        Some(spawner) => StdioRunner.run_with_spawner_as(
            &config,
            stdio_invocation,
            spawner,
            &handle.provider.run_as,
        )?,
        None => {
            let backend = OsProcessBackend::new();
            StdioRunner.run_with_spawner_as(
                &config,
                stdio_invocation,
                &backend,
                &handle.provider.run_as,
            )?
        }
    };
    let status = match (run.status, run.exit_code) {
        (StdioRunStatus::Completed, Some(0)) => "completed",
        (StdioRunStatus::Completed, _) => "failed",
        (StdioRunStatus::OutputLimitExceeded, _) => "output_limit_exceeded",
    }
    .to_owned();
    let mut audit = vec![format!("adapter.invoked:{}", handle.id.as_str())];
    audit.extend(run.audit);
    audit.push(format!(
        "stdio.exit_code:{}",
        run.exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "none".to_owned())
    ));
    audit.extend(credential_env.audit);
    if let Some(scope) = &credential_scope {
        audit.extend(scope.audit_entries());
    }

    Ok(AdapterInvokeReport {
        request_id,
        adapter_id: handle.id.clone(),
        transport: handle.transport,
        capability,
        status,
        output: format!(
            "{{\"transport\":\"stdio\",\"command\":{},\"process_id\":{},\"exit_code\":{},\"stdout\":{},\"stderr\":{},\"duration_ms\":{}}}",
            escape_json(&run.command),
            run.process_id,
            run.exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "null".to_owned()),
            provider_stream_summary_json(&run.stdout_stream),
            provider_stream_summary_json(&run.stderr_stream),
            run.duration_ms
        ),
        audit,
        trace,
    })
}

/// 定义 `ReaderMessage` 可取的状态。
enum ReaderMessage {
    /// 表示 `Output` 枚举分支。
    Output {
        /// 已完成脱敏和截断处理的单个输出流捕获结果。
        capture: ProviderStreamCapture,
    },
    /// 表示 `ReadError` 枚举分支。
    ReadError {
        /// 读取失败的输出流名称，例如 `stdout` 或 `stderr`。
        stream: String,
        /// 底层读取错误转换得到的可审计文本。
        error: String,
    },
}

/// 校验 `validate_invocation` 对应的约束，不满足时返回明确错误。
fn validate_invocation(
    config: &StdioRunnerConfig,
    invocation: &StdioInvocation,
) -> Result<(), EvaError> {
    if invocation.command.trim().is_empty() {
        return Err(EvaError::invalid_argument("stdio command cannot be empty"));
    }
    if !config.allowed_commands.contains(&invocation.command) {
        return Err(
            EvaError::permission_denied("stdio command is not allowlisted")
                .with_context("command", &invocation.command),
        );
    }
    if config.output_limit_bytes == 0 {
        return Err(EvaError::invalid_argument(
            "stdio output limit must be greater than zero",
        ));
    }
    Ok(())
}

/// 为单路管道启动读取线程；线程只通过通道返回完整采集或稳定错误，不修改共享状态。
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

/// 执行 `stream_config` 对应的处理逻辑。
fn stream_config(config: &StdioRunnerConfig, stream_name: &str) -> ProviderStreamConfig {
    let mut stream_config = ProviderStreamConfig::new(stream_name, config.output_limit_bytes)
        .with_preview_limit(config.preview_limit_bytes)
        .with_chunk_size(config.stream_chunk_size_bytes);
    if let (Some(root), Some(prefix)) = (&config.artifact_root, &config.artifact_key_prefix) {
        stream_config = stream_config.with_artifact(
            root.clone(),
            format!("{prefix}/{stream_name}"),
            "text/plain",
        );
    }
    stream_config
}

/// 执行 `stdio_audit` 对应的处理逻辑。
fn stdio_audit(
    config: &StdioRunnerConfig,
    stdout: &ProviderStreamCapture,
    stderr: &ProviderStreamCapture,
    limit_stream: Option<&str>,
) -> Vec<String> {
    let mut audit = vec![
        "transport:stdio".to_owned(),
        format!("output_limit_bytes:{}", config.output_limit_bytes),
    ];
    if let Some(stream) = limit_stream {
        audit.push(format!("stream:{stream}"));
    }
    audit.extend(provider_stream_audit(stdout));
    audit.extend(provider_stream_audit(stderr));
    audit
}

/// 尽力终止并等待完整 provider 边界，确保异常路径不会留下后台进程。
fn terminate_process(process: &mut ProviderProcessHandle) {
    // Give cooperative providers a short window to close their pipes before
    // the backend force-kills the complete process group/Job Object.
    if process
        .terminate_gracefully(Duration::from_millis(250))
        .is_err()
    {
        let _ = process.terminate();
    }
}

/// 表示 `CredentialEnvValues` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
struct CredentialEnvValues {
    /// 记录 `values` 字段对应的值。
    values: BTreeMap<String, String>,
    /// 记录 `audit` 字段对应的值。
    audit: Vec<String>,
}

/// 执行 `credential_env_values` 对应的处理逻辑。
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

/// 校验 `validate_input_size` 对应的约束，不满足时返回明确错误。
fn validate_input_size(handle: &AdapterHandle, input: &str) -> Result<(), EvaError> {
    if let Some(limit) = handle.max_prompt_bytes {
        if input.len() > limit {
            return Err(
                EvaError::conflict("stdio provider input exceeded prompt limit")
                    .with_context("adapter_id", handle.id.as_str())
                    .with_context("max_prompt_bytes", limit.to_string())
                    .with_context("actual_bytes", input.len().to_string()),
            );
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

/// 执行 `sensitive_values` 对应的处理逻辑。
fn sensitive_values<'a>(values: impl IntoIterator<Item = &'a String>) -> Vec<String> {
    values
        .into_iter()
        .filter(|value| !value.is_empty())
        .cloned()
        .collect()
}

/// 按 `escape_json` 的协议约定生成输出。
fn escape_json(value: &str) -> String {
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
    use eva_core::ErrorKind;

    /// 验证 `runner_denies_non_allowlisted_command` 场景下的预期行为。
    #[test]
    fn runner_denies_non_allowlisted_command() {
        let config = StdioRunnerConfig::new(["definitely-denied"], 1_000, 1024);
        let invocation = StdioInvocation::new("not-allowed");

        let error = StdioRunner.run(&config, invocation).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    /// 验证 `runner_times_out_slow_provider` 场景下的预期行为。
    #[test]
    fn runner_times_out_slow_provider() {
        let config = StdioRunnerConfig::new([test_command()], 1, 4096);
        let invocation = StdioInvocation::new(test_command()).with_args(sleep_args());

        let error = StdioRunner.run(&config, invocation).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Timeout);
    }

    /// 验证 `runner_reports_output_limit` 场景下的预期行为。
    #[test]
    fn runner_reports_output_limit() {
        let config = StdioRunnerConfig::new([test_command()], 5_000, 4);
        let invocation = StdioInvocation::new(test_command()).with_args(output_args("abcdef"));

        let report = StdioRunner.run(&config, invocation).unwrap();

        assert_eq!(report.status, StdioRunStatus::OutputLimitExceeded);
        assert_eq!(report.stdout, b"abcd");
    }

    /// 验证 `runner_completes_allowlisted_command_without_shell` 场景下的预期行为。
    #[test]
    fn runner_completes_allowlisted_command_without_shell() {
        let config = StdioRunnerConfig::new([test_command()], 5_000, 4096);
        let invocation = StdioInvocation::new(test_command()).with_args(output_args("ok"));

        let report = StdioRunner.run(&config, invocation).unwrap();

        assert_eq!(report.status, StdioRunStatus::Completed);
        assert_eq!(report.exit_code, Some(0));
        assert_eq!(String::from_utf8(report.stdout).unwrap(), "ok");
        assert!(report.audit.contains(&"shell:false".to_owned()));
    }

    /// 验证 `runner_redacts_injected_env_from_output_streams` 场景下的预期行为。
    #[test]
    fn runner_redacts_injected_env_from_output_streams() {
        let config = StdioRunnerConfig::new([test_command()], 5_000, 4096);
        let secret = "stdio-secret-redaction-test";
        let invocation = StdioInvocation::new(test_command())
            .with_args(output_args(secret))
            .with_env(BTreeMap::from([(
                "EVA_STDIO_SECRET".to_owned(),
                secret.to_owned(),
            )]));

        let report = StdioRunner.run(&config, invocation).unwrap();

        assert!(!String::from_utf8_lossy(&report.stdout).contains(secret));
        assert!(String::from_utf8_lossy(&report.stdout).contains("[REDACTED]"));
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

    /// 执行 `sleep_args` 对应的处理逻辑。
    #[cfg(windows)]
    fn sleep_args() -> Vec<&'static str> {
        vec![
            "-NoProfile",
            "-Command",
            "Start-Sleep -Milliseconds 200; Write-Output done",
        ]
    }

    /// 执行 `sleep_args` 对应的处理逻辑。
    #[cfg(not(windows))]
    fn sleep_args() -> Vec<&'static str> {
        vec!["-c", "sleep 0.2; printf done"]
    }

    /// 执行 `output_args` 对应的处理逻辑。
    #[cfg(windows)]
    fn output_args(output: &str) -> Vec<String> {
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            format!("[Console]::Out.Write('{output}')"),
        ]
    }

    /// 执行 `output_args` 对应的处理逻辑。
    #[cfg(not(windows))]
    fn output_args(output: &str) -> Vec<String> {
        vec!["-c".to_owned(), format!("printf '{output}'")]
    }
}
