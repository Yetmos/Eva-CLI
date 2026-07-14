//! 面向开发闭环的 Doctor 自检；只读取项目状态，不启动真实运行时。
//! Doctor checks for the V0.3 developer loop.

use crate::run::display_path;
use eva_config::{load_project_config, schema_paths};
use eva_runtime::RuntimeBuilder;
use std::path::Path;

/// `eva doctor` 的汇总结果，保留项目根目录和按执行顺序排列的检查项。
/// Result of `eva doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    /// 经过跨平台展示规范化的项目根目录。
    pub project_root: String,
    /// 环境、配置、schema 与运行时边界的逐项结果。
    pub checks: Vec<DoctorCheck>,
}

/// 单个环境或配置检查；字段同时服务于文本与稳定 JSON 输出。
/// One environment or configuration check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheck {
    /// 稳定的机器可读检查名称。
    pub name: String,
    /// 检查严重级别，决定最终命令是否视为失败。
    pub status: CheckStatus,
    /// 面向操作者的检查结论。
    pub message: String,
    /// 与问题相关的可选文件系统路径。
    pub path: Option<String>,
    /// 失败或警告时的可选修复建议。
    pub suggestion: Option<String>,
}

/// 文本与 JSON 共同使用的稳定 Doctor 状态；新增值会影响外部输出契约。
/// Stable doctor status used by text and JSON output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// 检查通过，无需操作者介入。
    Ok,
    /// 存在非阻塞风险，命令仍可成功。
    Warning,
    /// 存在阻塞问题，报告应被视为失败。
    Error,
}

impl CheckStatus {
    /// 返回稳定的小写状态码，避免展示层各自维护映射。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

impl DoctorReport {
    /// 报告中存在阻塞检查时返回 true；警告不会使 Doctor 失败。
    pub fn has_errors(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == CheckStatus::Error)
    }
}

/// 执行只读 Doctor 检查而不启动真实运行时。
///
/// 配置加载失败后立即返回，因为后续 schema、路径和 runtime builder 检查都依赖已验证配置；
/// 这种短路可避免在同一根因上产生大量误导性错误。
/// Runs V0.3 doctor checks without starting a real runtime.
pub fn doctor_project(project_root: &Path) -> DoctorReport {
    let mut report = DoctorReport {
        project_root: display_path(project_root),
        checks: Vec::new(),
    };

    push_path_check(
        &mut report,
        "workspace.root",
        project_root,
        project_root.is_dir(),
        "workspace root exists",
        "choose the repository root with --project",
    );
    push_path_check(
        &mut report,
        "workspace.cargo_manifest",
        &project_root.join("Cargo.toml"),
        project_root.join("Cargo.toml").is_file(),
        "Cargo.toml found",
        "run doctor from the Eva-CLI checkout root",
    );
    push_path_check(
        &mut report,
        "config.eva_yaml",
        &project_root.join("config").join("eva.yaml"),
        project_root.join("config").join("eva.yaml").is_file(),
        "config/eva.yaml found",
        "restore config/eva.yaml or pass the correct --project path",
    );

    let project = match load_project_config(project_root) {
        Ok(project) => {
            report.checks.push(DoctorCheck::ok(
                "config.load",
                "project configuration loaded and cross-file validation passed",
                Some(display_path(&project.eva_config_path)),
            ));
            project
        }
        Err(error) => {
            report.checks.push(DoctorCheck::error(
                "config.load",
                format!("configuration failed: {}", error.message()),
                None,
                Some("run `eva config validate --output json` for structured context".to_owned()),
            ));
            return report;
        }
    };

    let root_checks = [
        ("config.agent_dir", &project.roots.agent_dir, true),
        ("config.adapter_dir", &project.roots.adapter_dir, true),
        ("config.capability_dir", &project.roots.capability_dir, true),
        ("config.policy_dir", &project.roots.policy_dir, true),
        ("config.route_file", &project.roots.route_file, false),
        ("config.schema_dir", &project.roots.schema_dir, true),
    ];
    for (name, path, is_dir) in root_checks {
        let ok = if is_dir {
            path.is_dir()
        } else {
            path.is_file()
        };
        push_path_check(
            &mut report,
            name,
            path,
            ok,
            "configured path exists",
            "check the config.* path in config/eva.yaml",
        );
    }

    let schemas = schema_paths(&project.roots);
    for (name, path) in [
        ("schema.eva", schemas.eva),
        ("schema.agent", schemas.agent),
        ("schema.adapter", schemas.adapter),
        ("schema.capability", schemas.capability),
        ("schema.policy", schemas.policy),
        ("schema.routes", schemas.routes),
    ] {
        push_path_check(
            &mut report,
            name,
            &path,
            path.is_file(),
            "schema file exists",
            "restore config/schemas or update config.schema_dir",
        );
    }

    match RuntimeBuilder::new().build(&project) {
        Ok(runtime) => report.checks.push(DoctorCheck::ok(
            "runtime.noop_builder",
            format!(
                "no-op runtime summary is available with {} service boundaries",
                runtime.summary().services.len()
            ),
            None,
        )),
        Err(error) => report.checks.push(DoctorCheck::error(
            "runtime.noop_builder",
            format!("no-op runtime build failed: {}", error.message()),
            None,
            Some("run `eva inspect --output json` for runtime summary context".to_owned()),
        )),
    }

    push_path_check(
        &mut report,
        "lua.host_boundary",
        &project_root
            .join("crates")
            .join("eva-lua-host")
            .join("Cargo.toml"),
        project_root
            .join("crates")
            .join("eva-lua-host")
            .join("Cargo.toml")
            .is_file(),
        "Lua host crate boundary exists; V1.0 core controlled on_event execution and generation marker are available",
        "restore crates/eva-lua-host before running V1.0 core Lua execution",
    );

    let external_count = project
        .adapters
        .iter()
        .filter(|adapter| {
            matches!(
                adapter.transport.as_str(),
                "stdio" | "http" | "mcp" | "skill" | "hardware"
            )
        })
        .count();
    report.checks.push(DoctorCheck::warning(
        "external.adapters",
        format!(
            "{external_count} external adapter declarations found; V0.3 records placeholders but does not probe provider executables"
        ),
        None,
        Some("use V1.1 adapter probe once AdapterRuntime is implemented".to_owned()),
    ));

    report
}

impl DoctorCheck {
    /// 构造通过检查；通过项不附带修复建议。
    fn ok(name: impl Into<String>, message: impl Into<String>, path: Option<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Ok,
            message: message.into(),
            path,
            suggestion: None,
        }
    }

    /// 构造非阻塞警告，并允许提供下一步建议。
    fn warning(
        name: impl Into<String>,
        message: impl Into<String>,
        path: Option<String>,
        suggestion: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Warning,
            message: message.into(),
            path,
            suggestion,
        }
    }

    /// 构造阻塞错误，供 `DoctorReport::has_errors` 汇总失败状态。
    fn error(
        name: impl Into<String>,
        message: impl Into<String>,
        path: Option<String>,
        suggestion: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Error,
            message: message.into(),
            path,
            suggestion,
        }
    }
}

/// 将路径存在性判断规范化为 Doctor 检查，确保成功和失败都报告相同路径格式。
fn push_path_check(
    report: &mut DoctorReport,
    name: &str,
    path: &Path,
    ok: bool,
    ok_message: &str,
    suggestion: &str,
) {
    if ok {
        report
            .checks
            .push(DoctorCheck::ok(name, ok_message, Some(display_path(path))));
    } else {
        report.checks.push(DoctorCheck::error(
            name,
            "path is missing",
            Some(display_path(path)),
            Some(suggestion.to_owned()),
        ));
    }
}

#[cfg(test)]
/// Doctor 示例项目集成检查的回归测试。
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// 返回仓库根目录，供集成式 Doctor 测试使用真实示例配置。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    /// 验证仓库示例项目没有阻塞错误，同时保留预期的版本阶段警告。
    fn doctor_accepts_sample_project_with_only_v03_warnings() {
        let report = doctor_project(&workspace_root());

        assert!(!report.has_errors());
        assert!(report
            .checks
            .iter()
            .any(|check| check.name == "config.load"));
        assert!(report
            .checks
            .iter()
            .any(|check| check.status == CheckStatus::Warning));
    }
}
