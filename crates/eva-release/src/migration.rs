//! Migration guide and compatibility policy contracts.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationStep {
    pub id: String,
    pub summary: String,
    pub command: String,
    pub requires_manual_review: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatibilityPolicy {
    pub cli_json_envelope: String,
    pub exit_codes: String,
    pub config_schema: String,
    pub command_surface: String,
    pub deprecation_window: String,
    pub public_contracts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationGuide {
    pub from_version: String,
    pub to_version: String,
    pub status: String,
    pub breaking_changes: Vec<String>,
    pub steps: Vec<MigrationStep>,
    pub compatibility_policy: CompatibilityPolicy,
    pub audit: Vec<String>,
}

impl MigrationStep {
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
mod tests {
    use crate::ReleaseHardeningService;

    #[test]
    fn migration_has_no_breaking_changes_for_v1114_alpha() {
        let guide = ReleaseHardeningService::v15()
            .migration_guide("1.5.1", "1.11.4-alpha")
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
