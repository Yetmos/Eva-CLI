//! Stdio transport runner contract.

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "stdio command transport with separated command and args";

use crate::manifest::AdapterHandle;
use crate::runtime::{AdapterInvocation as RuntimeAdapterInvocation, AdapterInvokeReport};
use crate::supervisor::{redact_provider_session_tokens, validate_credential_scope_for_provider};
use eva_core::EvaError;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdioRunnerConfig {
    pub allowed_commands: BTreeSet<String>,
    pub timeout_ms: u64,
    pub output_limit_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdioInvocation {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub input: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdioRunReport {
    pub command: String,
    pub args: Vec<String>,
    pub status: StdioRunStatus,
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub duration_ms: u128,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioRunStatus {
    Completed,
    OutputLimitExceeded,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StdioRunner;

impl StdioRunnerConfig {
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
        }
    }
}

impl StdioInvocation {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            input: Vec::new(),
        }
    }

    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env = env;
        self
    }

    pub fn with_input(mut self, input: impl Into<Vec<u8>>) -> Self {
        self.input = input.into();
        self
    }
}

impl StdioRunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::OutputLimitExceeded => "output_limit_exceeded",
        }
    }
}

impl StdioRunner {
    pub fn run(
        &self,
        config: &StdioRunnerConfig,
        invocation: StdioInvocation,
    ) -> Result<StdioRunReport, EvaError> {
        validate_invocation(config, &invocation)?;

        let started_at = Instant::now();
        let sensitive_values = sensitive_values(invocation.env.values());
        let mut child = Command::new(&invocation.command)
            .args(&invocation.args)
            .envs(&invocation.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                EvaError::unavailable("failed to start stdio provider")
                    .with_context("command", &invocation.command)
                    .with_context("io_error", error.to_string())
            })?;

        if !invocation.input.is_empty() {
            let mut stdin = child.stdin.take().ok_or_else(|| {
                EvaError::internal("stdio provider stdin was not available")
                    .with_context("command", &invocation.command)
            })?;
            stdin.write_all(&invocation.input).map_err(|error| {
                EvaError::unavailable("failed to write stdio provider input")
                    .with_context("command", &invocation.command)
                    .with_context("io_error", error.to_string())
            })?;
        }
        drop(child.stdin.take());

        let stdout = child.stdout.take().ok_or_else(|| {
            EvaError::internal("stdio provider stdout was not available")
                .with_context("command", &invocation.command)
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            EvaError::internal("stdio provider stderr was not available")
                .with_context("command", &invocation.command)
        })?;
        let (sender, receiver) = mpsc::channel();
        spawn_reader("stdout", stdout, config.output_limit_bytes, sender.clone());
        spawn_reader("stderr", stderr, config.output_limit_bytes, sender);

        let timeout = Duration::from_millis(config.timeout_ms);
        let output_deadline = if timeout.is_zero() {
            None
        } else {
            Some(started_at + timeout)
        };
        let mut stdout_bytes = Vec::new();
        let mut stderr_bytes = Vec::new();
        for _ in 0..2 {
            let message = match output_deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        kill_child(&mut child);
                        return Err(EvaError::timeout("stdio provider timed out")
                            .with_context("command", &invocation.command)
                            .with_context("timeout_ms", config.timeout_ms.to_string()));
                    }
                    receiver.recv_timeout(deadline.saturating_duration_since(now))
                }
                None => receiver.recv().map_err(mpsc::RecvTimeoutError::from),
            }
            .map_err(|_| {
                kill_child(&mut child);
                EvaError::timeout("stdio provider timed out")
                    .with_context("command", &invocation.command)
                    .with_context("timeout_ms", config.timeout_ms.to_string())
            })?;

            match message {
                ReaderMessage::Output { stream, bytes } => match stream {
                    "stdout" => stdout_bytes = bytes,
                    "stderr" => stderr_bytes = bytes,
                    _ => {}
                },
                ReaderMessage::LimitExceeded { stream, bytes } => {
                    kill_child(&mut child);
                    let (stdout, stderr) = if stream == "stdout" {
                        (bytes, stderr_bytes)
                    } else {
                        (stdout_bytes, bytes)
                    };
                    return Ok(StdioRunReport {
                        command: invocation.command,
                        args: invocation.args,
                        status: StdioRunStatus::OutputLimitExceeded,
                        exit_code: None,
                        stdout: redact_bytes(stdout, &sensitive_values),
                        stderr: redact_bytes(stderr, &sensitive_values),
                        duration_ms: started_at.elapsed().as_millis(),
                        audit: vec![
                            "transport:stdio".to_owned(),
                            format!("output_limit_bytes:{}", config.output_limit_bytes),
                            format!("stream:{stream}"),
                        ],
                    });
                }
                ReaderMessage::ReadError { stream, error } => {
                    kill_child(&mut child);
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
                    status: StdioRunStatus::Completed,
                    exit_code: status.code(),
                    stdout: redact_bytes(stdout_bytes, &sensitive_values),
                    stderr: redact_bytes(stderr_bytes, &sensitive_values),
                    duration_ms: started_at.elapsed().as_millis(),
                    audit: vec![
                        "transport:stdio".to_owned(),
                        "shell:false".to_owned(),
                        "stdio:completed".to_owned(),
                    ],
                });
            }
            if !timeout.is_zero() && started_at.elapsed() >= timeout {
                kill_child(&mut child);
                return Err(EvaError::timeout("stdio provider timed out")
                    .with_context("command", &invocation.command)
                    .with_context("timeout_ms", config.timeout_ms.to_string()));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

pub fn invoke(
    handle: &AdapterHandle,
    invocation: RuntimeAdapterInvocation,
) -> Result<AdapterInvokeReport, EvaError> {
    let command = handle.command.as_deref().ok_or_else(|| {
        EvaError::invalid_argument("stdio adapter is missing command")
            .with_context("adapter_id", handle.id.as_str())
    })?;
    validate_input_size(handle, &invocation.input)?;

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
    let config = StdioRunnerConfig::new(
        [command.to_owned()],
        timeout_ms(handle),
        output_limit_bytes(handle),
    );
    let run = StdioRunner.run(
        &config,
        StdioInvocation::new(command)
            .with_args(handle.args.clone())
            .with_env(credential_env.values.clone())
            .with_input(invocation.input.into_bytes()),
    )?;
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
            "{{\"transport\":\"stdio\",\"command\":{},\"exit_code\":{},\"stdout\":{},\"stderr\":{},\"duration_ms\":{}}}",
            escape_json(&run.command),
            run.exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "null".to_owned()),
            escape_json(&String::from_utf8_lossy(&run.stdout)),
            escape_json(&String::from_utf8_lossy(&run.stderr)),
            run.duration_ms
        ),
        audit,
        trace,
    })
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct CredentialEnvValues {
    values: BTreeMap<String, String>,
    audit: Vec<String>,
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

fn timeout_ms(handle: &AdapterHandle) -> u64 {
    handle.timeout_ms.unwrap_or(30_000)
}

fn output_limit_bytes(handle: &AdapterHandle) -> usize {
    handle
        .output_limit_bytes
        .or(handle.max_prompt_bytes)
        .unwrap_or(64 * 1024)
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
    text = redact_provider_session_tokens(&text);
    text.into_bytes()
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::ErrorKind;

    #[test]
    fn runner_denies_non_allowlisted_command() {
        let config = StdioRunnerConfig::new(["definitely-denied"], 1_000, 1024);
        let invocation = StdioInvocation::new("not-allowed");

        let error = StdioRunner.run(&config, invocation).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    fn runner_times_out_slow_provider() {
        let config = StdioRunnerConfig::new([test_command()], 1, 4096);
        let invocation = StdioInvocation::new(test_command()).with_args(sleep_args());

        let error = StdioRunner.run(&config, invocation).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Timeout);
    }

    #[test]
    fn runner_reports_output_limit() {
        let config = StdioRunnerConfig::new([test_command()], 5_000, 4);
        let invocation = StdioInvocation::new(test_command()).with_args(output_args("abcdef"));

        let report = StdioRunner.run(&config, invocation).unwrap();

        assert_eq!(report.status, StdioRunStatus::OutputLimitExceeded);
        assert_eq!(report.stdout, b"abcd");
    }

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

    #[cfg(windows)]
    fn test_command() -> &'static str {
        "powershell"
    }

    #[cfg(not(windows))]
    fn test_command() -> &'static str {
        "sh"
    }

    #[cfg(windows)]
    fn sleep_args() -> Vec<&'static str> {
        vec![
            "-NoProfile",
            "-Command",
            "Start-Sleep -Milliseconds 200; Write-Output done",
        ]
    }

    #[cfg(not(windows))]
    fn sleep_args() -> Vec<&'static str> {
        vec!["-c", "sleep 0.2; printf done"]
    }

    #[cfg(windows)]
    fn output_args(output: &str) -> Vec<String> {
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            format!("[Console]::Out.Write('{output}')"),
        ]
    }

    #[cfg(not(windows))]
    fn output_args(output: &str) -> Vec<String> {
        vec!["-c".to_owned(), format!("printf '{output}'")]
    }
}
