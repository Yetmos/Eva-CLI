//! 从可信适配器清单发现 PATH 命令名。
//! PATH command discovery from trusted Adapter manifests.

use crate::normalizer::{DiscoveryCandidate, DiscoveryCandidateKind, DiscoveryTrust};
use crate::scanner::{DiscoveryScanContext, DiscoverySource};
use eva_config::{AdapterTransport, ProjectConfig};
use eva_core::{sha256_digest, EvaError};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{env, fs, thread, time::Duration};

/// 本来源的架构职责：从显式配置中发现可信的本地命令名。
pub const RESPONSIBILITY: &str = "discover trusted local commands from configured paths";

/// 基于标准输入输出适配器配置的 PATH 命令发现来源。
pub struct PathCommandDiscoverySource<'a> {
    /// 只读项目配置；扫描不会解析 PATH 或启动命令。
    project: &'a ProjectConfig,
}

impl<'a> PathCommandDiscoverySource<'a> {
    /// 为指定项目配置创建发现来源。
    pub fn new(project: &'a ProjectConfig) -> Self {
        Self { project }
    }
}

impl DiscoverySource for PathCommandDiscoverySource<'_> {
    /// 返回用于报告和增量缓存的稳定来源标识。
    fn source_id(&self) -> &str {
        "path_commands"
    }

    /// 返回本地清单扫描允许的最大耗时。
    fn timeout_ms(&self) -> u64 {
        250
    }

    /// 枚举标准输入输出适配器清单中的裸命令名。
    ///
    /// 包含路径分隔符的值被保留为拒绝候选项，因为本来源只记录应由后续受控 PATH
    /// 解析处理的命令名，不能借此接受任意文件路径。禁用适配器同样不会获得信任。
    fn scan(&self, context: &DiscoveryScanContext) -> Result<Vec<DiscoveryCandidate>, EvaError> {
        context.check()?;
        let mut candidates = Vec::new();
        for adapter in &self.project.adapters {
            if adapter.transport != AdapterTransport::Stdio {
                continue;
            }
            let Some(command) = adapter.extra_string("command") else {
                continue;
            };
            let mut candidate = DiscoveryCandidate::named(
                self.source_id(),
                DiscoveryCandidateKind::PathCommand,
                command,
                Some(adapter.id.clone()),
                DiscoveryTrust::ConfiguredAllowlist,
            );
            if !adapter.enabled {
                candidate = candidate.rejected("adapter manifest is disabled");
            } else if command.contains('/') || command.contains('\\') {
                candidate = candidate.rejected("PATH command source only records command names");
            } else {
                let hits = resolve_path(command);
                if hits.is_empty() {
                    candidate = candidate.rejected("PATH command was not found");
                } else {
                    let active = fs::canonicalize(&hits[0]).map_err(|e| {
                        EvaError::unavailable("canonicalize PATH command")
                            .with_context("io_error", e.to_string())
                    })?;
                    let allowed = adapter.nested_extra_string_list("permissions", "paths");
                    if !path_is_allowed(&active, &allowed) {
                        candidate = candidate.rejected(
                            "resolved PATH command is outside permissions.paths allowlist",
                        );
                    } else if !is_executable(&active) {
                        candidate = candidate.rejected("resolved PATH command is not executable");
                    } else {
                        let shadows = hits
                            .iter()
                            .skip(1)
                            .filter_map(|p| fs::canonicalize(p).ok())
                            .map(|p| sha256_digest(p.to_string_lossy().as_bytes()))
                            .collect();
                        let version = probe_version(&active, context)?;
                        candidate = candidate.with_path_probe(
                            sha256_digest(active.to_string_lossy().as_bytes()),
                            version,
                            shadows,
                        );
                    }
                }
            }
            candidates.push(candidate);
        }
        Ok(candidates)
    }
}

fn path_is_allowed(path: &Path, allowed: &[String]) -> bool {
    !allowed.is_empty()
        && allowed
            .iter()
            .any(|prefix| path.starts_with(Path::new(prefix)))
}

fn resolve_path(command: &str) -> Vec<PathBuf> {
    resolve_path_from(command, env::var_os("PATH").unwrap_or_default())
}
fn resolve_path_from(command: &str, path_value: std::ffi::OsString) -> Vec<PathBuf> {
    let mut names = vec![command.to_owned()];
    #[cfg(windows)]
    {
        if Path::new(command).extension().is_none() {
            names = env::var("PATHEXT")
                .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into())
                .split(';')
                .map(|e| format!("{command}{}", e.to_ascii_lowercase()))
                .collect();
        }
    }
    env::split_paths(&path_value)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .filter(|p| p.is_file())
        .collect()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
#[cfg(windows)]
fn is_executable(path: &Path) -> bool {
    path.extension()
        .and_then(|v| v.to_str())
        .map(|v| {
            matches!(
                v.to_ascii_lowercase().as_str(),
                "exe" | "cmd" | "bat" | "com"
            )
        })
        .unwrap_or(false)
}
#[cfg(not(any(unix, windows)))]
fn is_executable(_: &Path) -> bool {
    false
}

fn probe_version(path: &Path, context: &DiscoveryScanContext) -> Result<Option<String>, EvaError> {
    context.check()?;
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            EvaError::unavailable("start PATH version probe")
                .with_context("io_error", e.to_string())
        })?;
    loop {
        if let Some(status) = child.try_wait().map_err(|e| {
            EvaError::internal("poll PATH version probe").with_context("io_error", e.to_string())
        })? {
            let output = child.wait_with_output().map_err(|e| {
                EvaError::internal("read PATH version probe")
                    .with_context("io_error", e.to_string())
            })?;
            return Ok(status
                .success()
                .then(|| {
                    String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_owned()
                })
                .filter(|v| !v.is_empty()));
        }
        if context.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(EvaError::timeout("PATH version probe deadline expired"));
        }
        thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(test)]
/// PATH 命令来源的清单读取边界测试。
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 返回用于加载真实项目配置的工作区根目录。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    /// 验证只从清单产生命令候选项且不授予句柄。
    fn path_command_source_reads_manifest_commands_without_handles() {
        let project = load_project_config(workspace_root()).unwrap();
        let source = PathCommandDiscoverySource::new(&project);
        let candidates = source
            .scan(&DiscoveryScanContext::with_timeout(
                std::time::Duration::from_secs(1),
            ))
            .unwrap();

        assert!(candidates.iter().any(|candidate| {
            candidate.kind == DiscoveryCandidateKind::PathCommand
                && candidate.id == "path_command:codex-cli:codex"
        }));
        assert!(candidates.iter().all(|candidate| !candidate.handle_granted));
    }

    #[test]
    fn path_resolution_preserves_first_hit_and_reports_shadows() {
        let root = std::env::temp_dir().join(format!(
            "eva-path-source-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let name = if cfg!(windows) { "probe.exe" } else { "probe" };
        fs::write(first.join(name), b"x").unwrap();
        fs::write(second.join(name), b"x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for p in [first.join(name), second.join(name)] {
                let mut permissions = fs::metadata(&p).unwrap().permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(p, permissions).unwrap();
            }
        }
        let path = std::env::join_paths([&first, &second]).unwrap();
        let hits = resolve_path_from(name, path);
        assert_eq!(hits, vec![first.join(name), second.join(name)]);
        assert!(is_executable(&hits[0]));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn version_probe_honors_preexisting_cancellation() {
        let context = DiscoveryScanContext::with_timeout(Duration::ZERO);
        assert!(probe_version(Path::new("does-not-run"), &context).is_err());
    }

    #[test]
    fn path_allowlist_rejects_unconfigured_and_outside_paths() {
        assert!(!path_is_allowed(Path::new("C:/tools/probe.exe"), &[]));
        assert!(!path_is_allowed(
            Path::new("C:/other/probe.exe"),
            &["C:/tools".to_owned()]
        ));
        assert!(path_is_allowed(
            Path::new("C:/tools/probe.exe"),
            &["C:/tools".to_owned()]
        ));
    }
}
