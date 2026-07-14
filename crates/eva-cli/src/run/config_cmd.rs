//! 配置校验子命令：加载拆分清单并输出可审计的配置规模摘要。

use super::{
    display_path, json_array, json_string, parse_common_options, success_envelope, trace_for,
    write_error, write_error_kind, CommonOptions, OutputFormat, EXIT_CONFIG, EXIT_OK,
};
use eva_config::{load_project_config, schema_paths, ProjectConfig};
use eva_core::EvaError;
use eva_observability::TraceFields;
use std::io::Write;

#[derive(Debug, Clone, PartialEq, Eq)]
/// 已验证项目配置的只读摘要；计数区分总数与启用数。
struct ValidationReport {
    /// 规范化项目根路径。
    project_root: String,
    /// 实际加载的主配置路径。
    eva_config_path: String,
    /// 配置声明的运行环境。
    environment: String,
    /// 热重载开关。
    hot_reload: bool,
    /// Agent 清单总数。
    agents_total: usize,
    /// 已启用 Agent 数量。
    agents_enabled: usize,
    /// Adapter 清单总数。
    adapters_total: usize,
    /// 已启用 Adapter 数量。
    adapters_enabled: usize,
    /// Capability 清单总数。
    capabilities_total: usize,
    /// 已启用 Capability 数量。
    capabilities_enabled: usize,
    /// 已加载策略文件数量。
    policies_total: usize,
    /// 已验证路由规则数量。
    routes_total: usize,
    /// 校验使用的 schema 文件路径。
    schema_files: Vec<String>,
}

/// 解析唯一受支持的 `config validate` 子命令。
pub(super) fn parse_config_command(args: &[String]) -> Result<CommonOptions, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing config subcommand"))?;
    match subcommand.as_str() {
        "validate" => parse_common_options(rest),
        value => {
            Err(EvaError::unsupported("unknown config subcommand")
                .with_context("subcommand", value))
        }
    }
}

/// 加载并交叉校验项目配置；任何加载错误都固定映射为配置类退出码。
pub(super) fn execute_config_validate<W, E>(
    options: CommonOptions,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    let trace = trace_for("cli.config.validate");
    match load_project_config(&options.project_root) {
        Ok(project) => {
            let report = ValidationReport::from_project(&project);
            write_validation(stdout, options.output, &report, &trace)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            let exit_code = EXIT_CONFIG;
            write_error(
                stderr,
                options.output,
                "config.validate",
                exit_code,
                &error,
                &trace,
            )?;
            Ok(exit_code)
        }
    }
}

impl ValidationReport {
    /// 从已验证配置投影稳定摘要，避免输出层重新解释清单语义。
    fn from_project(project: &ProjectConfig) -> Self {
        let schemas = schema_paths(&project.roots);
        Self {
            project_root: display_path(&project.project_root),
            eva_config_path: display_path(&project.eva_config_path),
            environment: project.eva.runtime.env.clone(),
            hot_reload: project.eva.runtime.hot_reload,
            agents_total: project.agents.len(),
            agents_enabled: project.agents.iter().filter(|agent| agent.enabled).count(),
            adapters_total: project.adapters.len(),
            adapters_enabled: project
                .adapters
                .iter()
                .filter(|adapter| adapter.enabled)
                .count(),
            capabilities_total: project.capabilities.len(),
            capabilities_enabled: project
                .capabilities
                .iter()
                .filter(|capability| capability.enabled)
                .count(),
            policies_total: project.policies.len(),
            routes_total: project.routes.routes.len(),
            schema_files: vec![
                display_path(&schemas.eva),
                display_path(&schemas.agent),
                display_path(&schemas.adapter),
                display_path(&schemas.capability),
                display_path(&schemas.policy),
                display_path(&schemas.routes),
            ],
        }
    }
}

/// 按文本或 JSON 输出配置摘要；成功 JSON 始终使用统一信封。
fn write_validation<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &ValidationReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "OK config validated").map_err(write_error_kind)?;
            writeln!(writer, "project_root: {}", report.project_root).map_err(write_error_kind)?;
            writeln!(writer, "eva_config: {}", report.eva_config_path).map_err(write_error_kind)?;
            writeln!(writer, "environment: {}", report.environment).map_err(write_error_kind)?;
            writeln!(writer, "hot_reload: {}", report.hot_reload).map_err(write_error_kind)?;
            writeln!(
                writer,
                "agents: {} total, {} enabled",
                report.agents_total, report.agents_enabled
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "adapters: {} total, {} enabled",
                report.adapters_total, report.adapters_enabled
            )
            .map_err(write_error_kind)?;
            writeln!(
                writer,
                "capabilities: {} total, {} enabled",
                report.capabilities_total, report.capabilities_enabled
            )
            .map_err(write_error_kind)?;
            writeln!(writer, "policies: {}", report.policies_total).map_err(write_error_kind)?;
            writeln!(writer, "routes: {}", report.routes_total).map_err(write_error_kind)?;
            Ok(())
        }
        OutputFormat::Json => {
            let data = format!(
                "{{\"project_root\":{},\"eva_config_path\":{},\"environment\":{},\"hot_reload\":{},\"counts\":{{\"agents_total\":{},\"agents_enabled\":{},\"adapters_total\":{},\"adapters_enabled\":{},\"capabilities_total\":{},\"capabilities_enabled\":{},\"policies_total\":{},\"routes_total\":{}}},\"schema_files\":{}}}",
                json_string(&report.project_root),
                json_string(&report.eva_config_path),
                json_string(&report.environment),
                report.hot_reload,
                report.agents_total,
                report.agents_enabled,
                report.adapters_total,
                report.adapters_enabled,
                report.capabilities_total,
                report.capabilities_enabled,
                report.policies_total,
                report.routes_total,
                json_array(report.schema_files.iter().map(|path| json_string(path))),
            );
            writeln!(
                writer,
                "{}",
                success_envelope("config.validate", EXIT_OK, &data, trace)
            )
            .map_err(write_error_kind)
        }
    }
}
