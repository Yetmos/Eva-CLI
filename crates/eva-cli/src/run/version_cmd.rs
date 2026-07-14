//! 版本子命令：输出构建版本、发布标签、运行模式和受支持契约。

use super::{
    json_array, json_string, success_envelope, trace_for, write_error_kind, CommonOptions,
    OutputFormat, CLI_VERSION, EXIT_OK, RELEASE_CONTRACTS, RELEASE_LABEL, RELEASE_RUNTIME_MODE,
    RELEASE_STATUS,
};
use eva_core::EvaError;
use eva_observability::TraceFields;
use std::io::Write;

/// 执行无副作用版本查询并返回成功退出码。
pub(super) fn execute_version<W: Write>(
    options: CommonOptions,
    stdout: &mut W,
) -> Result<i32, EvaError> {
    let trace = trace_for("cli.version");
    write_version(stdout, options.output, &trace)?;
    Ok(EXIT_OK)
}

/// 按文本或 JSON 输出同一组版本事实，字段顺序属于稳定 CLI 契约。
fn write_version<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "eva {CLI_VERSION}").map_err(write_error_kind)?;
            writeln!(writer, "release: {RELEASE_LABEL}").map_err(write_error_kind)?;
            writeln!(writer, "status: {RELEASE_STATUS}").map_err(write_error_kind)?;
            writeln!(writer, "runtime_mode: {RELEASE_RUNTIME_MODE}").map_err(write_error_kind)?;
            writeln!(writer, "contracts: {}", RELEASE_CONTRACTS.join(", "))
                .map_err(write_error_kind)
        }
        OutputFormat::Json => {
            let data = format!(
                "{{\"version\":{},\"release\":{},\"status\":{},\"runtime_mode\":{},\"contracts\":{}}}",
                json_string(CLI_VERSION),
                json_string(RELEASE_LABEL),
                json_string(RELEASE_STATUS),
                json_string(RELEASE_RUNTIME_MODE),
                json_array(RELEASE_CONTRACTS.iter().copied().map(json_string))
            );
            writeln!(
                writer,
                "{}",
                success_envelope("version", EXIT_OK, &data, trace)
            )
            .map_err(write_error_kind)
        }
    }
}
