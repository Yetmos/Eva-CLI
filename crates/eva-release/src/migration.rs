//! 迁移指南与兼容性政策契约。
//! Migration guide and compatibility policy contracts.

/// 迁移指南中的一个可执行或人工审核步骤。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationStep {
    /// 步骤的稳定标识。
    pub id: String,
    /// 面向操作者的迁移目的说明。
    pub summary: String,
    /// 可运行的命令或人工操作提示。
    pub command: String,
    /// 是否必须由人工确认结果。
    pub requires_manual_review: bool,
}

/// 一个发布系列承诺保持稳定的公共兼容性边界。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatibilityPolicy {
    /// CLI JSON 响应信封的稳定性承诺。
    pub cli_json_envelope: String,
    /// 进程退出码语义承诺。
    pub exit_codes: String,
    /// 项目配置清单的兼容性承诺。
    pub config_schema: String,
    /// 既有命令表面的可用性承诺。
    pub command_surface: String,
    /// 引入破坏性变化前的弃用窗口要求。
    pub deprecation_window: String,
    /// 被政策覆盖的具体命令和数据契约。
    pub public_contracts: Vec<String>,
}

/// 从一个版本升级到另一个版本的兼容性结论与操作指南。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationGuide {
    /// 当前安装版本。
    pub from_version: String,
    /// 目标发布版本。
    pub to_version: String,
    /// 迁移兼容性状态。
    pub status: String,
    /// 已确认的破坏性变化列表。
    pub breaking_changes: Vec<String>,
    /// 升级前后需要完成的步骤。
    pub steps: Vec<MigrationStep>,
    /// 本次结论采用的兼容性政策。
    pub compatibility_policy: CompatibilityPolicy,
    /// 指南生成与兼容性判断审计记录。
    pub audit: Vec<String>,
}

impl MigrationStep {
    /// 创建一个迁移步骤描述。
    pub fn new(
        id: impl Into<String>,
        summary: impl Into<String>,
        command: impl Into<String>,
        requires_manual_review: bool,
    ) -> Self {
        Self {
            id: id.into(),
            summary: summary.into(),
            command: command.into(),
            requires_manual_review,
        }
    }
}

impl CompatibilityPolicy {
    /// 返回 V1.x 发布系列的稳定公共契约基线。
    pub fn v15() -> Self {
        Self {
            cli_json_envelope: "ok, command, exit_code, data/error, and trace remain stable"
                .to_owned(),
            exit_codes: "0, 1, 2, 3, 4, 5, and 64 keep their V1.0 meanings".to_owned(),
            config_schema: "existing sample project manifests remain loadable without migration"
                .to_owned(),
            command_surface: "V1.5 only adds release commands; V1.0-V1.4 commands remain available"
                .to_owned(),
            deprecation_window:
                "breaking CLI or manifest changes require one documented release window".to_owned(),
            public_contracts: vec![
                "eva --version".to_owned(),
                "eva version --output json".to_owned(),
                "eva doctor/config/inspect".to_owned(),
                "eva run --example basic".to_owned(),
                "eva task status/logs/cancel".to_owned(),
                "eva adapter/mcp/skill/discovery".to_owned(),
                "eva memory context".to_owned(),
                "eva hardware list/probe/bind".to_owned(),
                "eva backup/snapshot/restore/upgrade".to_owned(),
                "eva release check/security/perf/migration".to_owned(),
            ],
        }
    }
}

#[cfg(test)]
/// 当前 alpha 发布的兼容性政策回归测试。
mod tests {
    use crate::ReleaseHardeningService;

    #[test]
    /// 验证当前版本迁移指南不声明破坏性变化并覆盖发布命令。
    fn migration_has_no_breaking_changes_for_v1115_alpha() {
        let guide = ReleaseHardeningService::v15()
            .migration_guide("1.5.1", "1.11.5-alpha")
            .unwrap();

        assert_eq!(guide.status, "compatible");
        assert!(guide.breaking_changes.is_empty());
        assert!(guide
            .compatibility_policy
            .public_contracts
            .iter()
            .any(|contract| contract.contains("release check")));
    }
}
