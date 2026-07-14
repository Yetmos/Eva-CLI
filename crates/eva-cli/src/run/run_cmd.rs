//! 基础示例运行子命令：装配内存运行时，并可将任务快照写入持久化后端。

use super::{
    exit_code_for_error, json_array, json_string, option_json, parse_common_options,
    parse_u64_option, parse_usize_option, success_envelope, task_cmd, trace_for, trace_json,
    write_error, write_error_kind, CommonOptions, OutputFormat, EXIT_OK, EXIT_RUNTIME_UNAVAILABLE,
    EXIT_USAGE,
};
use eva_config::load_project_config;
use eva_core::{EvaError, InvokeStatus};
use eva_observability::TraceFields;
use eva_runtime::{BasicRunOptions, BasicRunReport, RuntimeBuilder};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
/// `eva run` 的已解析执行选项。
pub(super) struct RunOptions {
    /// 项目根和输出格式。
    common: CommonOptions,
    /// 要运行的内置示例；当前仅支持 `basic`。
    example: Option<String>,
    /// 覆盖运行时默认请求/任务 ID 的可选值。
    task_id: Option<String>,
    /// 可选持久化后端根，用于保存任务快照。
    durable_backend: Option<PathBuf>,
    /// 单次运行 timeout；None 明确禁用超时。
    timeout_ms: Option<u64>,
    /// 是否在运行开始时请求取消。
    cancel_requested: bool,
    /// 最大重试尝试次数，解析时至少归一化为 1。
    retry_attempts: usize,
    /// 是否重放已就绪 dead-letter。
    replay_dead_letters: bool,
}

/// 解析运行示例、任务、超时、重试和持久化选项。
pub(super) fn parse_run_options(args: &[String]) -> Result<RunOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut example = None;
    let mut task_id = None;
    let mut durable_backend = None;
    let mut timeout_ms = Some(30_000);
    let mut cancel_requested = false;
    let mut retry_attempts = 1;
    let mut replay_dead_letters = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--example" => {
                index += 1;
                let value = args.get(index).ok_or_else(|| {
                    EvaError::invalid_argument("missing value for example option")
                })?;
                example = Some(value.clone());
            }
            "--task-id" | "--task" => {
                index += 1;
                let value = args.get(index).ok_or_else(|| {
                    EvaError::invalid_argument("missing value for task id option")
                })?;
                eva_core::RequestId::parse(value)?;
                task_id = Some(value.clone());
            }
            "--durable-backend" | "--durable-backend-root" => {
                index += 1;
                durable_backend = Some(PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| {
                            EvaError::invalid_argument("missing value for durable backend option")
                        })?
                        .as_str(),
                ));
            }
            "--timeout-ms" => {
                index += 1;
                let value = args.get(index).ok_or_else(|| {
                    EvaError::invalid_argument("missing value for timeout option")
                })?;
                timeout_ms = Some(parse_u64_option("timeout_ms", value)?);
            }
            "--no-timeout" => timeout_ms = None,
            "--cancel" => cancel_requested = true,
            "--retry-attempts" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| EvaError::invalid_argument("missing value for retry option"))?;
                retry_attempts = parse_usize_option("retry_attempts", value)?.max(1);
            }
            "--replay-dead-letters" => replay_dead_letters = true,
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    Ok(RunOptions {
        common: parse_common_options(&passthrough)?,
        example,
        task_id,
        durable_backend,
        timeout_ms,
        cancel_requested,
        retry_attempts,
        replay_dead_letters,
    })
}

/// 执行 run 命令并兜底映射意外返回的结构化错误。
pub(super) fn execute_run<W, E>(
    options: RunOptions,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    let trace = trace_for("cli.run");
    match execute_run_inner(options, stdout, stderr, &trace) {
        Ok(exit_code) => Ok(exit_code),
        Err(error) => {
            let exit_code = exit_code_for_error(&error);
            write_error(stderr, OutputFormat::Text, "run", exit_code, &error, &trace)?;
            Ok(exit_code)
        }
    }
}

/// 执行已支持示例并保持每种失败路径的输出格式。
///
/// `basic` 成功后先持久化任务快照再报告成功，确保用户看到成功时状态可查询；未知示例
/// 属于 usage 错误，缺少示例则表示当前运行时能力不可用，两者使用不同退出码。
fn execute_run_inner<W, E>(
    options: RunOptions,
    stdout: &mut W,
    stderr: &mut E,
    trace: &TraceFields,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match options.example.as_deref() {
        Some("basic") => {
            let project_root = options.common.project_root.join("examples").join("basic");
            let mut run_options = BasicRunOptions {
                timeout_ms: options.timeout_ms,
                cancel_requested: options.cancel_requested,
                retry_attempts: options.retry_attempts,
                replay_dead_letters: options.replay_dead_letters,
                ..BasicRunOptions::default()
            };
            if let Some(task_id) = &options.task_id {
                run_options.request_id = eva_core::RequestId::parse(task_id)?;
            }
            match load_project_config(&project_root).and_then(|project| {
                let runtime = RuntimeBuilder::in_memory_v10().build(&project)?;
                runtime
                    .run_basic(&project, run_options)
                    .map(|report| (project, runtime, report))
            }) {
                Ok((_project, _runtime, report)) => {
                    task_cmd::write_task_snapshot(
                        &options.common.project_root,
                        options.durable_backend.as_deref(),
                        &report,
                    )?;
                    write_run(stdout, options.common.output, &report, trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => {
                    let exit_code = exit_code_for_error(&error);
                    write_error(
                        stderr,
                        options.common.output,
                        "run",
                        exit_code,
                        &error,
                        trace,
                    )?;
                    Ok(exit_code)
                }
            }
        }
        Some(example) => {
            let error = EvaError::unsupported("unknown run example")
                .with_context("example", example)
                .with_context("supported", "basic");
            let exit_code = EXIT_USAGE;
            write_error(
                stderr,
                options.common.output,
                "run",
                exit_code,
                &error,
                trace,
            )?;
            Ok(exit_code)
        }
        None => {
            let error = EvaError::unsupported("eva run requires an example in V1.0 core")
                .with_context("suggestion", "use `eva run --example basic`");
            let exit_code = EXIT_RUNTIME_UNAVAILABLE;
            write_error(
                stderr,
                options.common.output,
                "run",
                exit_code,
                &error,
                trace,
            )?;
            Ok(exit_code)
        }
    }
}

/// 输出基础运行报告的任务、事件、投递、Agent 与 capability 结果。
fn write_run<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &BasicRunReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "OK run example=basic").map_err(write_error_kind)?;
            writeln!(writer, "project_root: {}", report.project_root).map_err(write_error_kind)?;
            writeln!(
                writer,
                "runtime: mode={} generation={}",
                report.runtime_mode, report.generation_id
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "task: id={} status={} attempts={}/{}",
                report.task.task_id,
                report.task.status.as_str(),
                report.task.attempts,
                report.task.retry_policy.max_attempts
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "event: {} topic={} sequence={}",
                report.event_id, report.topic, report.receipt.sequence
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "deliveries:").map_err(write_error_kind)?;
            for delivery in &report.deliveries {
                writeln!(
                    writer,
                    "  - agent={} delivery={}",
                    delivery.agent_id,
                    delivery.delivery.as_str()
                )
                .map_err(write_error_kind)?;
            }
            writeln!(writer, "agent_runs:").map_err(write_error_kind)?;
            for run in &report.agent_runs {
                writeln!(
                    writer,
                    "  - agent={} status={} handler_status={}",
                    run.agent_id,
                    run.status.as_str(),
                    run.handler_status.as_deref().unwrap_or("")
                )
                .map_err(write_error_kind)?;
            }
            if let Some(response) = &report.capability_response {
                writeln!(
                    writer,
                    "capability: status={} output={}",
                    invoke_status(response.status()),
                    response
                        .output()
                        .and_then(|output| output.as_text())
                        .unwrap_or("")
                )
                .map_err(write_error_kind)?;
            }
            Ok(())
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("run", EXIT_OK, &run_report_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

/// 将完整基础运行报告编码为稳定 JSON，包括 Lua 观察、审计和任务快照。
fn run_report_json(report: &BasicRunReport) -> String {
    let deliveries = report.deliveries.iter().map(|delivery| {
        format!(
            "{{\"agent_id\":{},\"delivery\":{}}}",
            json_string(delivery.agent_id.as_str()),
            json_string(delivery.delivery.as_str())
        )
    });
    let agent_runs = report.agent_runs.iter().map(|run| {
        let error = run
            .error
            .as_ref()
            .map(|error| json_string(error.message()))
            .unwrap_or_else(|| "null".to_owned());
        format!(
            "{{\"agent_id\":{},\"event_id\":{},\"topic\":{},\"status\":{},\"attempts\":{},\"handler_status\":{},\"output\":{},\"error\":{}}}",
            json_string(run.agent_id.as_str()),
            json_string(run.event_id.as_str()),
            json_string(run.topic.as_str()),
            json_string(run.status.as_str()),
            run.attempts,
            option_json(run.handler_status.as_deref()),
            option_json(run.output.as_deref()),
            error
        )
    });
    let lua_results = report.lua_results.iter().map(|result| {
        format!(
            "{{\"agent_id\":{},\"status\":{},\"topic\":{},\"note\":{},\"capability\":{},\"capability_input\":{}}}",
            json_string(result.agent_id.as_str()),
            json_string(&result.status),
            json_string(result.topic.as_str()),
            option_json(result.note.as_deref()),
            result
                .capability
                .as_ref()
                .map(|capability| json_string(capability.as_str()))
                .unwrap_or_else(|| "null".to_owned()),
            option_json(result.capability_input.as_deref())
        )
    });
    let lua_observability = report.lua_observability.iter().map(|observation| {
        let fields = observation.fields.iter().map(|(key, value)| {
            format!(
                "{{\"key\":{},\"value\":{}}}",
                json_string(key),
                json_string(value)
            )
        });
        format!(
            "{{\"action\":{},\"outcome\":{},\"message\":{},\"fields\":{},\"trace\":{}}}",
            json_string(observation.action.as_str()),
            json_string(observation.outcome.as_str()),
            option_json(observation.message.as_deref()),
            json_array(fields),
            trace_json(&observation.trace)
        )
    });
    let capability_response = report
        .capability_response
        .as_ref()
        .map(capability_response_json)
        .unwrap_or_else(|| "null".to_owned());
    format!(
        "{{\"runtime_mode\":{},\"generation_id\":{},\"project_root\":{},\"task\":{},\"event_id\":{},\"topic\":{},\"receipt\":{{\"event_id\":{},\"sequence\":{},\"topic\":{},\"target\":{}}},\"deliveries\":{},\"agent_runs\":{},\"lua_results\":{},\"lua_observability\":{},\"lua_generation\":{{\"generation_id\":{},\"script_count\":{}}},\"capability_response\":{},\"audit\":{}}}",
        json_string(&report.runtime_mode),
        json_string(&report.generation_id),
        json_string(&report.project_root),
        task_cmd::task_snapshot_json_from_report(report),
        json_string(&report.event_id),
        json_string(&report.topic),
        json_string(report.receipt.event_id.as_str()),
        report.receipt.sequence,
        json_string(report.receipt.topic.as_str()),
        json_string(&format!("{:?}", report.receipt.target)),
        json_array(deliveries),
        json_array(agent_runs),
        json_array(lua_results),
        json_array(lua_observability),
        json_string(report.lua_generation.generation_id.as_str()),
        report.lua_generation.script_count,
        capability_response,
        json_array(report.audit.iter().map(|entry| json_string(entry)))
    )
}

/// 将可选 capability 调用响应压缩为 CLI JSON 字段。
fn capability_response_json(response: &eva_core::InvokeResponse) -> String {
    format!(
        "{{\"request_id\":{},\"status\":{},\"output\":{},\"error\":{}}}",
        json_string(response.request_id().as_str()),
        json_string(invoke_status(response.status())),
        response
            .output()
            .and_then(|output| output.as_text())
            .map(json_string)
            .unwrap_or_else(|| "null".to_owned()),
        response
            .error()
            .map(|error| json_string(error.message()))
            .unwrap_or_else(|| "null".to_owned())
    )
}

/// 将 InvokeStatus 映射为稳定小写状态文本。
fn invoke_status(status: InvokeStatus) -> &'static str {
    match status {
        InvokeStatus::Accepted => "accepted",
        InvokeStatus::Completed => "completed",
        InvokeStatus::Failed => "failed",
        InvokeStatus::Cancelled => "cancelled",
        InvokeStatus::Timeout => "timeout",
    }
}
