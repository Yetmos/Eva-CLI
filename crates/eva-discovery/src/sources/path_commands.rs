//! 从可信适配器清单发现 PATH 命令名。
//! PATH command discovery from trusted Adapter manifests.

use crate::normalizer::{DiscoveryCandidate, DiscoveryCandidateKind, DiscoveryTrust};
use crate::scanner::DiscoverySource;
use eva_config::{AdapterTransport, ProjectConfig};
use eva_core::EvaError;

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
    fn scan(&self) -> Result<Vec<DiscoveryCandidate>, EvaError> {
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
            }
            candidates.push(candidate);
        }
        Ok(candidates)
    }
}

#[cfg(test)]
/// PATH 命令来源的清单读取边界测试。
mod tests {
    use super::*;
    use eva_config::load_project_config;
    use std::path::{Path, PathBuf};

    /// 返回用于加载真实项目配置的工作区根目录。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    /// 验证只从清单产生命令候选项且不授予句柄。
    fn path_command_source_reads_manifest_commands_without_handles() {
        let project = load_project_config(workspace_root()).unwrap();
        let source = PathCommandDiscoverySource::new(&project);
        let candidates = source.scan().unwrap();

        assert!(candidates.iter().any(|candidate| {
            candidate.kind == DiscoveryCandidateKind::PathCommand
                && candidate.id == "path_command:codex-cli:codex"
        }));
        assert!(candidates.iter().all(|candidate| !candidate.handle_granted));
    }
}
