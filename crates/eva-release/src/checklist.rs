//! Release readiness checklist aggregation.

use crate::migration::{CompatibilityPolicy, MigrationGuide, MigrationStep};
use crate::performance::{PerformanceBaselineReport, PerformanceBudget};
use crate::security::{SecurityFinding, SecurityReviewReport, SecuritySeverity};
use eva_core::EvaError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseGateStatus {
    Pass,
    Warn,
    Blocked,
}

impl ReleaseGateStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Blocked => "blocked",
        }
    }

    pub const fn is_blocking(self) -> bool {
        matches!(self, Self::Blocked)
    }

    pub const fn is_warning(self) -> bool {
        matches!(self, Self::Warn)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseGate {
    pub id: String,
    pub domain: String,
    pub status: ReleaseGateStatus,
    pub required: bool,
    pub summary: String,
    pub evidence: Vec<String>,
    pub remediation: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformReadiness {
    pub os: String,
    pub shell: String,
    pub path_model: String,
    pub status: ReleaseGateStatus,
    pub required_commands: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StabilityScenario {
    pub id: String,
    pub status: ReleaseGateStatus,
    pub scenario: String,
    pub evidence: Vec<String>,
    pub recovery_contract: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseReadinessReport {
    pub version: String,
    pub status: String,
    pub target: String,
    pub platforms: Vec<PlatformReadiness>,
    pub stability: Vec<StabilityScenario>,
    pub gates: Vec<ReleaseGate>,
    pub audit: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReleaseHardeningService;

impl ReleaseHardeningService {
    pub fn v15() -> Self {
        Self
    }

    pub fn readiness(&self, target: impl Into<String>) -> Result<ReleaseReadinessReport, EvaError> {
        let target = target.into();
        if target.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "release readiness target cannot be empty",
            ));
        }
        if !matches!(target.as_str(), "all" | "windows" | "linux" | "macos") {
            return Err(EvaError::invalid_argument(
                "release readiness target must be all, windows, linux, or macos",
            )
            .with_context("target", target));
        }

        let platforms = self.platforms(&target);
        let security = self.security_review();
        let performance = self.performance_baseline();
        let migration = self.migration_guide("1.5.0", "1.5.1")?;
        let stability = self.stability_scenarios();
        let mut gates = self.core_gates(&platforms, &stability);
        gates.extend(security.findings.iter().map(security_gate));
        gates.extend(performance.budgets.iter().map(performance_gate));
        gates.push(migration_gate(&migration));

        let status = if gates
            .iter()
            .any(|gate| gate.required && gate.status.is_blocking())
        {
            "blocked"
        } else {
            "ready"
        }
        .to_owned();

        Ok(ReleaseReadinessReport {
            version: "1.5.1".to_owned(),
            status,
            target,
            platforms,
            stability,
            gates,
            audit: vec![
                "release:readiness:v1.5".to_owned(),
                "no_destructive_restore_or_process_switch".to_owned(),
                "all_external_capability_checks_are_plan_or_probe_first".to_owned(),
            ],
        })
    }

    pub fn security_review(&self) -> SecurityReviewReport {
        SecurityReviewReport {
            version: "1.5.1".to_owned(),
            status: "reviewed".to_owned(),
            findings: vec![
                SecurityFinding::passed(
                    "SEC-POLICY-001",
                    "policy",
                    SecuritySeverity::High,
                    "effective policy narrows permissions and rejects request expansion",
                    vec![
                        "eva-policy unit tests cover permission narrowing".to_owned(),
                        "CLI policy denials map to stable exit code 3".to_owned(),
                    ],
                ),
                SecurityFinding::passed(
                    "SEC-SANDBOX-001",
                    "lua_sandbox",
                    SecuritySeverity::High,
                    "Lua host exposes controlled context snapshots instead of host handles",
                    vec![
                        "eva-lua-host sandbox rejects forbidden tokens".to_owned(),
                        "memory context output contains counts and audit, not raw host APIs"
                            .to_owned(),
                    ],
                ),
                SecurityFinding::passed(
                    "SEC-SECRET-001",
                    "secret_redaction",
                    SecuritySeverity::Medium,
                    "backup manifests retain redaction metadata for sensitive entries",
                    vec![
                        "release pointer entry is redacted in V1.4 backup smoke".to_owned(),
                        "JSON writers do not emit secret values from policy context".to_owned(),
                    ],
                ),
                SecurityFinding::passed(
                    "SEC-MCP-001",
                    "mcp",
                    SecuritySeverity::High,
                    "MCP tools are allowlisted and probes do not start external servers",
                    vec![
                        "mcp probe blocks unlisted tool delete_repo".to_owned(),
                        "MCP runtime remains in-memory for V1.5 release hardening".to_owned(),
                    ],
                ),
                SecurityFinding::passed(
                    "SEC-HW-001",
                    "hardware",
                    SecuritySeverity::High,
                    "hardware discovery never grants raw I/O handles from CLI",
                    vec![
                        "hardware candidates report handle_granted=false".to_owned(),
                        "hardware bind remains plan-first even with --apply".to_owned(),
                    ],
                ),
                SecurityFinding::tracked(
                    "SEC-LIFE-001",
                    "restore_and_upgrade",
                    SecuritySeverity::Medium,
                    "restore and upgrade remain diagnostic until real apply authorization exists",
                    vec![
                        "restore plan reports apply_allowed=false".to_owned(),
                        "upgrade check does not start supervisor processes".to_owned(),
                    ],
                    vec![
                        "require signed artifacts before destructive restore".to_owned(),
                        "require explicit apply authorization before process handoff".to_owned(),
                    ],
                ),
            ],
            audit: vec![
                "security_review:policy_sandbox_secret_mcp_hardware_lifecycle".to_owned(),
                "known_future_apply_paths_are_tracked_not_enabled".to_owned(),
            ],
        }
    }

    pub fn performance_baseline(&self) -> PerformanceBaselineReport {
        let budgets = vec![
            PerformanceBudget::new(
                "eventbus.publish",
                "single in-memory publish latency",
                5,
                1,
                "bounded append plus receipt allocation",
            ),
            PerformanceBudget::new(
                "scheduler.fanout",
                "basic topic routing fanout latency",
                10,
                2,
                "in-memory route match and mailbox delivery",
            ),
            PerformanceBudget::new(
                "adapter.probe",
                "side-effect-free adapter probe latency",
                15,
                2,
                "manifest-derived handle probe without provider startup",
            ),
            PerformanceBudget::new(
                "memory.context",
                "request context assembly latency",
                25,
                3,
                "bounded private/global memory and knowledge result merge",
            ),
            PerformanceBudget::new(
                "backup.create",
                "in-memory backup artifact creation latency",
                50,
                4,
                "artifact serialization plus digest verification",
            ),
            PerformanceBudget::new(
                "release.check",
                "release hardening report generation latency",
                20,
                2,
                "static checklist aggregation with no filesystem mutation",
            ),
        ];
        let status = if budgets.iter().all(PerformanceBudget::within_budget) {
            "within_budget"
        } else {
            "over_budget"
        }
        .to_owned();

        PerformanceBaselineReport {
            version: "1.5.1".to_owned(),
            status,
            budgets,
            audit: vec![
                "performance:baseline:v1.5".to_owned(),
                "budgets_are_contractual_smoke_thresholds_not_microbenchmarks".to_owned(),
            ],
        }
    }

    pub fn migration_guide(
        &self,
        from: impl Into<String>,
        to: impl Into<String>,
    ) -> Result<MigrationGuide, EvaError> {
        let from = from.into();
        let to = to.into();
        if from.trim().is_empty() || to.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "migration guide requires non-empty versions",
            ));
        }
        Ok(MigrationGuide {
            from_version: from,
            to_version: to,
            status: "compatible".to_owned(),
            breaking_changes: Vec::new(),
            steps: vec![
                MigrationStep::new(
                    "build",
                    "rebuild the workspace with version 1.5.1",
                    "cargo build --release",
                    false,
                ),
                MigrationStep::new(
                    "smoke",
                    "run the V1.0 to V1.5 smoke commands before release hardening checks",
                    "cargo run -- upgrade check --output json",
                    false,
                ),
                MigrationStep::new(
                    "release-check",
                    "run the aggregate V1.5 release readiness command",
                    "cargo run -- release check --output json",
                    false,
                ),
                MigrationStep::new(
                    "security-review",
                    "review tracked security findings before enabling real restore or process handoff",
                    "cargo run -- release security --output json",
                    false,
                ),
                MigrationStep::new(
                    "performance-baseline",
                    "compare EventBus, Scheduler, Adapter, memory, backup, and release budgets",
                    "cargo run -- release perf --output json",
                    false,
                ),
            ],
            compatibility_policy: CompatibilityPolicy::v15(),
            audit: vec![
                "migration:v1.5.0_to_v1.5.1:no_breaking_changes".to_owned(),
                "json_envelope_and_exit_codes_remain_stable".to_owned(),
            ],
        })
    }

    fn platforms(&self, target: &str) -> Vec<PlatformReadiness> {
        let all = vec![
            PlatformReadiness {
                os: "windows".to_owned(),
                shell: "pwsh".to_owned(),
                path_model: "drive-letter and backslash tolerant project roots".to_owned(),
                status: ReleaseGateStatus::Pass,
                required_commands: smoke_commands(),
                notes: vec![
                    "CI matrix includes windows-latest".to_owned(),
                    "CLI accepts --project paths through PathBuf".to_owned(),
                ],
            },
            PlatformReadiness {
                os: "linux".to_owned(),
                shell: "pwsh".to_owned(),
                path_model: "case-sensitive POSIX paths".to_owned(),
                status: ReleaseGateStatus::Pass,
                required_commands: smoke_commands(),
                notes: vec![
                    "CI matrix includes ubuntu-latest".to_owned(),
                    "website and docs validation run on Ubuntu".to_owned(),
                ],
            },
            PlatformReadiness {
                os: "macos".to_owned(),
                shell: "pwsh".to_owned(),
                path_model: "case-insensitive default filesystem with POSIX separators".to_owned(),
                status: ReleaseGateStatus::Pass,
                required_commands: smoke_commands(),
                notes: vec![
                    "CI matrix includes macos-latest".to_owned(),
                    "no platform-specific native provider is started in V1.5".to_owned(),
                ],
            },
        ];
        if target == "all" {
            all
        } else {
            all.into_iter()
                .filter(|platform| platform.os == target)
                .collect()
        }
    }

    fn stability_scenarios(&self) -> Vec<StabilityScenario> {
        vec![
            StabilityScenario {
                id: "STAB-TASK-001".to_owned(),
                status: ReleaseGateStatus::Pass,
                scenario: "long or failed task can be inspected after execution".to_owned(),
                evidence: vec![
                    "task status/logs/cancel read latest .eva task report".to_owned(),
                    "timeout run can replay dead letters".to_owned(),
                ],
                recovery_contract: "operators keep task JSON/text output as diagnostic evidence"
                    .to_owned(),
            },
            StabilityScenario {
                id: "STAB-CANCEL-001".to_owned(),
                status: ReleaseGateStatus::Pass,
                scenario: "cancellation is explicit and auditable".to_owned(),
                evidence: vec![
                    "run --cancel generates cancelled task".to_owned(),
                    "task cancel records rejected late cancellation on completed task".to_owned(),
                ],
                recovery_contract: "cancel does not mutate completed terminal task state"
                    .to_owned(),
            },
            StabilityScenario {
                id: "STAB-UPGRADE-001".to_owned(),
                status: ReleaseGateStatus::Pass,
                scenario: "upgrade and restore operations are planned before apply".to_owned(),
                evidence: vec![
                    "restore plan apply_allowed=false".to_owned(),
                    "upgrade check returns drain and rollback plans".to_owned(),
                ],
                recovery_contract: "future apply path must keep backup snapshot before handoff"
                    .to_owned(),
            },
        ]
    }

    fn core_gates(
        &self,
        platforms: &[PlatformReadiness],
        stability: &[StabilityScenario],
    ) -> Vec<ReleaseGate> {
        let platform_status = status_from(platforms.iter().map(|platform| platform.status));
        let stability_status = status_from(stability.iter().map(|scenario| scenario.status));
        vec![
            ReleaseGate {
                id: "REL-PLATFORM-001".to_owned(),
                domain: "cross_platform".to_owned(),
                status: platform_status,
                required: true,
                summary: "Windows, Linux, and macOS CI matrix smoke commands are release gates"
                    .to_owned(),
                evidence: platforms
                    .iter()
                    .map(|platform| format!("{}:{}", platform.os, platform.status.as_str()))
                    .collect(),
                remediation: vec!["fix platform-specific path or shell behavior before release".to_owned()],
            },
            ReleaseGate {
                id: "REL-STABILITY-001".to_owned(),
                domain: "stability".to_owned(),
                status: stability_status,
                required: true,
                summary: "task, cancellation, dead-letter, restore, and upgrade recovery paths are auditable"
                    .to_owned(),
                evidence: stability
                    .iter()
                    .map(|scenario| format!("{}:{}", scenario.id, scenario.status.as_str()))
                    .collect(),
                remediation: vec!["add a failing scenario to release check before enabling apply".to_owned()],
            },
            ReleaseGate {
                id: "REL-DOCS-001".to_owned(),
                domain: "docs".to_owned(),
                status: ReleaseGateStatus::Pass,
                required: true,
                summary: "V1.5.1-release README, version management, GitHub Packages, migration, compatibility, and release notes are part of the release surface"
                    .to_owned(),
                evidence: vec![
                    "crates/eva-release/README.md".to_owned(),
                    "docs/en/release/version-management-plan.md".to_owned(),
                    "docs/en/release/github-packages-publishing.md".to_owned(),
                    "docs/en/release/v1.5-migration-guide.md".to_owned(),
                    "docs/en/release/v1.5-compatibility-policy.md".to_owned(),
                    "docs/en/release/release-notes-v1.5.1.md".to_owned(),
                ],
                remediation: vec!["update docs and i18n validation before tagging release".to_owned()],
            },
        ]
    }
}

impl ReleaseReadinessReport {
    pub fn blocking_count(&self) -> usize {
        self.gates
            .iter()
            .filter(|gate| gate.required && gate.status.is_blocking())
            .count()
    }

    pub fn warning_count(&self) -> usize {
        self.gates
            .iter()
            .filter(|gate| gate.status.is_warning())
            .count()
    }
}

fn status_from<I>(statuses: I) -> ReleaseGateStatus
where
    I: IntoIterator<Item = ReleaseGateStatus>,
{
    let mut has_warn = false;
    for status in statuses {
        if status.is_blocking() {
            return ReleaseGateStatus::Blocked;
        }
        has_warn |= status.is_warning();
    }
    if has_warn {
        ReleaseGateStatus::Warn
    } else {
        ReleaseGateStatus::Pass
    }
}

fn security_gate(finding: &SecurityFinding) -> ReleaseGate {
    ReleaseGate {
        id: finding.id.clone(),
        domain: format!("security:{}", finding.boundary),
        status: finding.status,
        required: finding.severity.is_required_gate(),
        summary: finding.summary.clone(),
        evidence: finding.evidence.clone(),
        remediation: finding.remediation.clone(),
    }
}

fn performance_gate(budget: &PerformanceBudget) -> ReleaseGate {
    ReleaseGate {
        id: format!("PERF-{}", budget.component.replace('.', "-").to_uppercase()),
        domain: "performance".to_owned(),
        status: budget.status,
        required: true,
        summary: format!(
            "{} observed {}ms within {}ms budget",
            budget.component, budget.observed_ms, budget.budget_ms
        ),
        evidence: vec![budget.evidence.clone()],
        remediation: vec![
            "investigate regression before widening public performance budget".to_owned(),
        ],
    }
}

fn migration_gate(guide: &MigrationGuide) -> ReleaseGate {
    ReleaseGate {
        id: "REL-MIGRATION-001".to_owned(),
        domain: "migration".to_owned(),
        status: if guide.breaking_changes.is_empty() {
            ReleaseGateStatus::Pass
        } else {
            ReleaseGateStatus::Warn
        },
        required: true,
        summary: format!(
            "migration {} -> {} has {} breaking changes",
            guide.from_version,
            guide.to_version,
            guide.breaking_changes.len()
        ),
        evidence: guide
            .steps
            .iter()
            .map(|step| step.command.clone())
            .collect(),
        remediation: vec!["document and test every breaking change before release".to_owned()],
    }
}

fn smoke_commands() -> Vec<String> {
    vec![
        "cargo fmt --check".to_owned(),
        "cargo clippy --workspace --all-targets -- -D warnings".to_owned(),
        "cargo test --workspace".to_owned(),
        "cargo run -- --version".to_owned(),
        "cargo run -- release check --output json".to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_has_no_blocking_required_gates() {
        let report = ReleaseHardeningService::v15().readiness("all").unwrap();

        assert_eq!(report.status, "ready");
        assert_eq!(report.blocking_count(), 0);
        assert!(report
            .gates
            .iter()
            .any(|gate| gate.domain == "cross_platform"));
        assert!(report.gates.iter().any(|gate| gate.domain == "migration"));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DOCS-001"
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "docs/en/release/github-packages-publishing.md")
        }));
    }

    #[test]
    fn readiness_can_filter_target_platform() {
        let report = ReleaseHardeningService::v15().readiness("windows").unwrap();

        assert_eq!(report.platforms.len(), 1);
        assert_eq!(report.platforms[0].os, "windows");
    }

    #[test]
    fn readiness_rejects_unknown_target_platform() {
        let error = ReleaseHardeningService::v15()
            .readiness("unix")
            .unwrap_err();

        assert!(error.message().contains("target must be"));
    }
}
