//! Doctor 子命令适配层：把只读检查报告映射为稳定退出码和输出格式。

use super::{
    json_array, json_string, parse_common_options, success_envelope, trace_for, write_error_kind,
    CommonOptions, OutputFormat, EXIT_CONFIG, EXIT_OK,
};
use crate::doctor::{doctor_project, CheckStatus, DoctorReport};
use eva_core::EvaError;
use eva_observability::TraceFields;
use std::io::Write;

/// Doctor 仅接受公共项目与输出选项。
pub(super) fn parse_doctor_options(args: &[String]) -> Result<CommonOptions, EvaError> {
    parse_common_options(args)
}

/// 执行 Doctor 并在存在 Error 检查时返回配置类退出码；Warning 不阻塞成功。
pub(super) fn execute_doctor<W: Write>(
    options: CommonOptions,
    stdout: &mut W,
) -> Result<i32, EvaError> {
    let trace = trace_for("cli.doctor");
    let report = doctor_project(&options.project_root);
    let exit_code = if report.has_errors() {
        EXIT_CONFIG
    } else {
        EXIT_OK
    };
    write_doctor(stdout, options.output, exit_code, &report, &trace)?;
    Ok(exit_code)
}

/// 输出完整 Doctor 报告；JSON 模式同时给出错误和警告计数供自动化判断。
fn write_doctor<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    exit_code: i32,
    report: &DoctorReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva doctor").map_err(write_error_kind)?;
            writeln!(writer, "project_root: {}", report.project_root).map_err(write_error_kind)?;
            for check in &report.checks {
                writeln!(
                    writer,
                    "[{}] {} - {}",
                    check.status.as_str(),
                    check.name,
                    check.message
                )
                .map_err(write_error_kind)?;
                if let Some(path) = &check.path {
                    writeln!(writer, "  path: {path}").map_err(write_error_kind)?;
                }
                if let Some(suggestion) = &check.suggestion {
                    writeln!(writer, "  suggestion: {suggestion}").map_err(write_error_kind)?;
                }
            }
            Ok(())
        }
        OutputFormat::Json => {
            let checks = report
                .checks
                .iter()
                .map(|check| {
                    let mut fields = vec![
                        format!("\"name\":{}", json_string(&check.name)),
                        format!("\"status\":{}", json_string(check.status.as_str())),
                        format!("\"message\":{}", json_string(&check.message)),
                    ];
                    if let Some(path) = &check.path {
                        fields.push(format!("\"path\":{}", json_string(path)));
                    }
                    if let Some(suggestion) = &check.suggestion {
                        fields.push(format!("\"suggestion\":{}", json_string(suggestion)));
                    }
                    format!("{{{}}}", fields.join(","))
                })
                .collect::<Vec<_>>();
            let data = format!(
                "{{\"project_root\":{},\"checks\":{},\"error_count\":{},\"warning_count\":{}}}",
                json_string(&report.project_root),
                json_array(checks),
                report
                    .checks
                    .iter()
                    .filter(|check| check.status == CheckStatus::Error)
                    .count(),
                report
                    .checks
                    .iter()
                    .filter(|check| check.status == CheckStatus::Warning)
                    .count(),
            );
            writeln!(
                writer,
                "{}",
                success_envelope("doctor", exit_code, &data, trace)
            )
            .map_err(write_error_kind)
        }
    }
}
