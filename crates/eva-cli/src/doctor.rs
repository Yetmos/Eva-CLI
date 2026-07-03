//! Doctor checks for the V0.3 developer loop.

use crate::run::display_path;
use eva_config::{load_project_config, schema_paths};
use eva_runtime::RuntimeBuilder;
use std::path::Path;

/// Result of `eva doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub project_root: String,
    pub checks: Vec<DoctorCheck>,
}

/// One environment or configuration check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheck {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
    pub path: Option<String>,
    pub suggestion: Option<String>,
}

/// Stable doctor status used by text and JSON output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Ok,
    Warning,
    Error,
}

impl CheckStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

impl DoctorReport {
    pub fn has_errors(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == CheckStatus::Error)
    }
}

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
        "Lua host crate boundary exists; script execution is planned for V0.4",
        "restore crates/eva-lua-host before implementing V0.4 Lua execution",
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
    fn ok(name: impl Into<String>, message: impl Into<String>, path: Option<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Ok,
            message: message.into(),
            path,
            suggestion: None,
        }
    }

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
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
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
