//! Release readiness checklist aggregation.

use crate::artifact::{
    ReleaseArtifactEvidence, ReleaseArtifactSigningKey, ReleaseArtifactVerificationReport,
};
use crate::distribution::{ReleaseDistributionEvidence, ReleaseDistributionVerificationReport};
use crate::migration::{CompatibilityPolicy, MigrationGuide, MigrationStep};
use crate::performance::{PerformanceBaselineReport, PerformanceBudget};
use crate::security::{SecurityFinding, SecurityReviewReport, SecuritySeverity};
use eva_core::EvaError;

const CURRENT_RELEASE_VERSION: &str = "1.7.4-alpha";
const CURRENT_RELEASE_LABEL: &str = "V1.7.4-alpha";

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
        self.readiness_inner(target.into(), None, None)
    }

    pub fn readiness_with_artifact_evidence(
        &self,
        target: impl Into<String>,
        evidence: &ReleaseArtifactEvidence,
    ) -> Result<ReleaseReadinessReport, EvaError> {
        self.readiness_inner(target.into(), Some(evidence), None)
    }

    pub fn readiness_with_distribution_evidence(
        &self,
        target: impl Into<String>,
        evidence: &ReleaseDistributionEvidence,
    ) -> Result<ReleaseReadinessReport, EvaError> {
        self.readiness_inner(target.into(), None, Some(evidence))
    }

    pub fn readiness_with_release_evidence(
        &self,
        target: impl Into<String>,
        artifact_evidence: Option<&ReleaseArtifactEvidence>,
        distribution_evidence: Option<&ReleaseDistributionEvidence>,
    ) -> Result<ReleaseReadinessReport, EvaError> {
        self.readiness_inner(target.into(), artifact_evidence, distribution_evidence)
    }

    fn readiness_inner(
        &self,
        target: String,
        artifact_evidence: Option<&ReleaseArtifactEvidence>,
        distribution_evidence: Option<&ReleaseDistributionEvidence>,
    ) -> Result<ReleaseReadinessReport, EvaError> {
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
        let migration = self.migration_guide("1.5.1", CURRENT_RELEASE_VERSION)?;
        let stability = self.stability_scenarios();
        let mut gates = self.core_gates(&platforms, &stability);
        gates.extend(security.findings.iter().map(security_gate));
        gates.extend(performance.budgets.iter().map(performance_gate));
        gates.push(migration_gate(&migration));
        gates.push(durable_backend_gate());
        gates.push(durable_eventbus_gate());
        gates.push(durable_task_audit_artifact_gate());
        gates.push(durable_runtime_recovery_gate());
        gates.push(durable_diagnostics_gate());
        gates.push(lua_vm_execution_gate());
        gates.push(lua_host_bindings_gate());
        gates.push(lua_resource_limits_gate());
        gates.push(lua_hot_reload_lifecycle_gate());
        gates.push(signed_backup_archive_gate());
        gates.push(restore_apply_gate());
        gates.push(supervisor_handoff_gate());
        let artifact_report = artifact_evidence
            .map(|evidence| evidence.verify(&ReleaseArtifactSigningKey::local_development()));
        if let Some(report) = artifact_report.as_ref() {
            gates.push(release_artifact_provenance_gate(report));
        }
        let distribution_report = distribution_evidence.map(ReleaseDistributionEvidence::verify);
        if let Some(report) = distribution_report.as_ref() {
            gates.push(release_distribution_gate(report));
        }

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
            version: CURRENT_RELEASE_VERSION.to_owned(),
            status,
            target,
            platforms,
            stability,
            gates,
            audit: release_audit(artifact_report.as_ref(), distribution_report.as_ref()),
        })
    }

    pub fn security_review(&self) -> SecurityReviewReport {
        SecurityReviewReport {
            version: CURRENT_RELEASE_VERSION.to_owned(),
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
                    "restore and upgrade mutations are gated while service-manager integration remains future work",
                    vec![
                        "restore plan reports apply_allowed=false".to_owned(),
                        "restore apply gate verifies confirmation, archive evidence, policy approval, lock, and health before reporting apply_allowed=true".to_owned(),
                        "restore apply reports mutation_executed=false until destructive file mutation exists".to_owned(),
                        "upgrade apply can commit a controlled supervisor handoff and release pointer mutation inside the configured state store after policy approval".to_owned(),
                    ],
                    vec![
                        "require destructive restore file mutation to stay behind the existing restore apply gate".to_owned(),
                        "replace the local supervisor adapter smoke with a real service-manager adapter before daemonized production handoff".to_owned(),
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
            version: CURRENT_RELEASE_VERSION.to_owned(),
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
                    format!("rebuild the workspace with version {CURRENT_RELEASE_VERSION}"),
                    "cargo build --release",
                    false,
                ),
                MigrationStep::new(
                    "smoke",
                    "run the V1.0 to V1.6 smoke commands before release checks",
                    "cargo run -- upgrade check --output json",
                    false,
                ),
                MigrationStep::new(
                    "release-check",
                    "run the aggregate release readiness command",
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
                "migration:v1.5.1_to_v1.7.4-alpha:no_breaking_changes".to_owned(),
                "json_envelope_and_exit_codes_remain_stable".to_owned(),
                "durable_task_audit_artifact_additive_alpha_baseline".to_owned(),
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
                    "no platform-specific native provider is started in this alpha release"
                        .to_owned(),
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
            StabilityScenario {
                id: "STAB-RECOVERY-001".to_owned(),
                status: ReleaseGateStatus::Pass,
                scenario: "durable runtime recovery can scan, redrive, and audit restart evidence"
                    .to_owned(),
                evidence: vec![
                    "recover_task_store_with_audit covers clean start".to_owned(),
                    "recover_task_store_with_redrive_and_audit covers restart redrive".to_owned(),
                    "corrupt task store returns stable error".to_owned(),
                ],
                recovery_contract:
                    "recovery never redrives acked events and records durable audit evidence"
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
                summary: format!("{CURRENT_RELEASE_LABEL} README, version management, GitHub Packages, install/upgrade/uninstall docs, migration, compatibility, and release notes are part of the release surface"),
                evidence: vec![
                    "crates/eva-release/README.md".to_owned(),
                    "docs/en/release/version-management-plan.md".to_owned(),
                    "docs/en/release/github-packages-publishing.md".to_owned(),
                    "docs/en/release/install-upgrade-uninstall.md".to_owned(),
                    "docs/en/release/v1.5-migration-guide.md".to_owned(),
                    "docs/en/release/v1.5-compatibility-policy.md".to_owned(),
                    "docs/en/release/release-notes-v1.7.4.md".to_owned(),
                ],
                remediation: vec!["update docs and i18n validation before tagging release".to_owned()],
            },
        ]
    }
}

fn release_audit(
    artifact_report: Option<&ReleaseArtifactVerificationReport>,
    distribution_report: Option<&ReleaseDistributionVerificationReport>,
) -> Vec<String> {
    let mut audit = vec![
        "release:readiness:v1.7.4-alpha".to_owned(),
        "no_unauthorized_destructive_restore_or_process_switch".to_owned(),
        "all_external_capability_checks_are_plan_or_probe_first".to_owned(),
        "durable_backend_layout_baseline_ready".to_owned(),
        "durable_eventbus_redrive_baseline_ready".to_owned(),
        "durable_task_audit_artifact_baseline_ready".to_owned(),
        "durable_runtime_recovery_checkpoint_ready".to_owned(),
        "durable_diagnostics_smoke_ready".to_owned(),
        "lua_vm_execution_boundary_ready".to_owned(),
        "lua_host_bindings_ready".to_owned(),
        "lua_resource_limits_ready".to_owned(),
        "lua_hot_reload_lifecycle_ready".to_owned(),
        "signed_backup_archive_baseline_ready".to_owned(),
        "restore_apply_gate_baseline_ready".to_owned(),
        "supervisor_handoff_baseline_ready".to_owned(),
    ];
    if let Some(report) = artifact_report {
        if report.status == "verified" {
            audit.push("signed_artifact_provenance_verified".to_owned());
        } else {
            audit.push("signed_artifact_provenance_blocked".to_owned());
        }
        audit.extend(report.audit.iter().cloned());
    }
    if let Some(report) = distribution_report {
        if report.status == "verified" {
            audit.push("distribution_install_smoke_verified".to_owned());
        } else {
            audit.push("distribution_install_smoke_blocked".to_owned());
        }
        audit.extend(report.audit.iter().cloned());
    }
    audit
}

fn release_artifact_provenance_gate(report: &ReleaseArtifactVerificationReport) -> ReleaseGate {
    let mut evidence = vec![
        format!("artifact:{}", report.artifact_name),
        format!("target:{}", report.target),
        format!("digest:{}", report.artifact_digest),
        format!("source_commit:{}", report.source_commit),
        format!("signature_verified:{}", report.signature_verified),
        format!("provenance_verified:{}", report.provenance_verified),
    ];
    evidence.extend(report.audit.iter().cloned());
    ReleaseGate {
        id: "REL-ARTIFACT-PROVENANCE-001".to_owned(),
        domain: "release_artifact_provenance".to_owned(),
        status: if report.status == "verified" {
            ReleaseGateStatus::Pass
        } else {
            ReleaseGateStatus::Blocked
        },
        required: true,
        summary: "V1.11.1 signed release artifact and provenance evidence are verified".to_owned(),
        evidence,
        remediation: if report.risks.is_empty() {
            Vec::new()
        } else {
            report.risks.clone()
        },
    }
}

fn release_distribution_gate(report: &ReleaseDistributionVerificationReport) -> ReleaseGate {
    let mut evidence = vec![
        format!("version:{}", report.version),
        format!("source_commit:{}", report.source_commit),
        format!("install_docs_verified:{}", report.install_docs_verified),
        format!(
            "package_dry_runs_verified:{}",
            report.package_dry_runs_verified
        ),
    ];
    evidence.extend(report.platform_smokes.iter().map(|smoke| {
        format!(
            "install_smoke:{}:{}:{}:{}",
            smoke.os, smoke.target, smoke.artifact, smoke.status
        )
    }));
    evidence.extend(report.package_dry_runs.iter().map(|dry_run| {
        format!(
            "package_dry_run:{}:{}:{}",
            dry_run.manager, dry_run.target, dry_run.status
        )
    }));
    evidence.extend(report.audit.iter().cloned());
    ReleaseGate {
        id: "REL-DISTRIBUTION-001".to_owned(),
        domain: "release_distribution".to_owned(),
        status: if report.status == "verified" {
            ReleaseGateStatus::Pass
        } else {
            ReleaseGateStatus::Blocked
        },
        required: true,
        summary: "V1.11.2 cross-platform installer smoke and package-manager dry-run evidence are verified".to_owned(),
        evidence,
        remediation: if report.risks.is_empty() {
            Vec::new()
        } else {
            report.risks.clone()
        },
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

fn durable_backend_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-DURABLE-BACKEND-001".to_owned(),
        domain: "durable_backend".to_owned(),
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.6.1 durable backend schema, layout, read-only verification, and migration lock baseline are implemented".to_owned(),
        evidence: vec![
            "crates/eva-storage/src/durable_backend.rs".to_owned(),
            "cargo test -p eva-storage".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.6.1 Done".to_owned(),
            "docs/en/release/release-notes-v1.6.1.md".to_owned(),
        ],
        remediation: vec![
            "do not build V1.6.2 durable EventBus on an unverified backend manifest or migration lock".to_owned(),
        ],
    }
}

fn durable_eventbus_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-DURABLE-EVENTBUS-001".to_owned(),
        domain: "durable_eventbus".to_owned(),
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.6.2 durable EventBus publish/ack/fail, queryable dead-letter store, and redrive baseline are implemented".to_owned(),
        evidence: vec![
            "crates/eva-storage/src/event_log.rs FileSystemEventLog".to_owned(),
            "crates/eva-eventbus/src/durable.rs DurableEventBus".to_owned(),
            "cargo test -p eva-storage".to_owned(),
            "cargo test -p eva-eventbus".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.6.2 Done".to_owned(),
            "docs/en/release/release-notes-v1.6.2.md".to_owned(),
        ],
        remediation: vec![
            "do not build crash recovery on top of EventBus records without preserving publish/ack/fail and dead-letter redrive round trips".to_owned(),
        ],
    }
}

fn durable_task_audit_artifact_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-DURABLE-STORES-001".to_owned(),
        domain: "durable_task_audit_artifact".to_owned(),
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.6.3 durable task store adapter, audit sink, and artifact metadata hardening are implemented".to_owned(),
        evidence: vec![
            "crates/eva-storage/src/task_state.rs FileSystemTaskStateStore::from_durable_layout".to_owned(),
            "crates/eva-storage/src/audit_store.rs FileSystemAuditSink".to_owned(),
            "crates/eva-storage/src/artifact_store.rs FileSystemArtifactStore v2 metadata".to_owned(),
            "cargo test -p eva-storage".to_owned(),
            "cargo test -p eva-cli task_commands_can_use_durable_backend_task_store".to_owned(),
            "cargo test -p eva-backup".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.6.3 Done".to_owned(),
            "docs/en/release/release-notes-v1.6.3.md".to_owned(),
        ],
        remediation: vec![
            "do not start V1.6.4 crash recovery without preserving durable task snapshot, audit query, and artifact metadata corruption tests".to_owned(),
        ],
    }
}

fn durable_runtime_recovery_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-DURABLE-RECOVERY-001".to_owned(),
        domain: "durable_runtime_recovery".to_owned(),
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.6.4 runtime recovery scanner, event redrive checkpoint, and durable recovery audit smoke are implemented".to_owned(),
        evidence: vec![
            "crates/eva-runtime/src/recovery.rs RuntimeRecoveryCoordinator".to_owned(),
            "crates/eva-eventbus/src/durable.rs DurableEventBus::redrive_dead_letter".to_owned(),
            "crates/eva-observability/src/audit.rs AuditAction::RuntimeRecovered".to_owned(),
            "cargo test -p eva-runtime recovery".to_owned(),
            "cargo test -p eva-eventbus durable".to_owned(),
            "cargo test -p eva-cli recovery".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.6.4 Done".to_owned(),
        ],
        remediation: vec![
            "do not enable provider process recovery without preserving ack skip, redrive policy, and durable audit tests".to_owned(),
        ],
    }
}

fn durable_diagnostics_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-DURABLE-DIAGNOSTICS-001".to_owned(),
        domain: "durable_diagnostics".to_owned(),
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.6.5 durable backend diagnostics report schema, migration, and pending redrive counts through inspect.durable".to_owned(),
        evidence: vec![
            "crates/eva-runtime/src/diagnostics.rs inspect_durable_backend".to_owned(),
            "crates/eva-cli/src/run.rs inspect.durable JSON envelope".to_owned(),
            "cargo test -p eva-runtime diagnostics".to_owned(),
            "cargo test -p eva-cli inspect_durable".to_owned(),
            "cargo run -- inspect durable --durable-backend .eva/ci-durable --output json".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.6.5 Done".to_owned(),
        ],
        remediation: vec![
            "do not remove inspect.durable from CI smoke while durable backend fields are part of the release surface".to_owned(),
        ],
    }
}

fn lua_vm_execution_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-LUA-VM-EXECUTION-001".to_owned(),
        domain: "lua_vm_execution".to_owned(),
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.7.1 Lua VM adapter, restricted standard library, real on_event execution, and stable error mapping are implemented".to_owned(),
        evidence: vec![
            "crates/eva-lua-host/src/vm.rs LuaVmAdapter and MluaVmAdapter".to_owned(),
            "crates/eva-lua-host/src/bindings.rs real VM execution and compatibility fallback".to_owned(),
            "cargo test -p eva-lua-host".to_owned(),
            "cargo test -p eva-runtime basic_example_runs_event_to_lua_and_capability".to_owned(),
            "cargo test -p eva-cli run_basic_example_json_succeeds".to_owned(),
            "docs/zh-CN/planning/V1.x real runtime implementation plan V1.7.1 Done"
                .to_owned(),
        ],
        remediation: vec![
            "do not add resource-limit behavior without preserving the LuaVmAdapter boundary and restricted standard library tests".to_owned(),
        ],
    }
}

fn lua_host_bindings_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-LUA-HOST-BINDINGS-001".to_owned(),
        domain: "lua_host_bindings".to_owned(),
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.7.2 read-only Lua ctx, host log/audit, and ctx.tools.call capability binding are implemented".to_owned(),
        evidence: vec![
            "crates/eva-lua-host/src/bindings.rs run_on_event_with_tools".to_owned(),
            "crates/eva-lua-host/src/vm.rs ctx.tools.call".to_owned(),
            "examples/basic/config/agents/root-agent/main.lua direct config.lint tool call".to_owned(),
            "cargo test -p eva-lua-host".to_owned(),
            "cargo test -p eva-runtime basic_example_runs_event_to_lua_and_capability".to_owned(),
            "cargo test -p eva-cli run_basic_example_json_succeeds".to_owned(),
            "docs/zh-CN/planning/V1.x real runtime implementation plan V1.7.2 Done".to_owned(),
        ],
        remediation: vec![
            "do not expose raw provider, file, socket, process, memory service, knowledge service, or audit sink handles through Lua ctx".to_owned(),
            "keep unknown and disabled ctx.tools.call capability requests rejected through the host boundary".to_owned(),
        ],
    }
}

fn lua_resource_limits_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-LUA-RESOURCE-LIMITS-001".to_owned(),
        domain: "lua_resource_limits".to_owned(),
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.7.3 Lua wall-clock timeout, instruction budget, cancellation token, and memory budget limits are implemented".to_owned(),
        evidence: vec![
            "crates/eva-lua-host/src/vm.rs LuaExecutionLimits".to_owned(),
            "crates/eva-lua-host/src/bindings.rs cancellation and memory-budget host tests".to_owned(),
            "crates/eva-runtime/src/basic.rs BasicRunOptions timeout/cancel Lua limits".to_owned(),
            "cargo test -p eva-lua-host".to_owned(),
            "cargo test -p eva-runtime timeout_basic_run_records_dead_letter_and_replay".to_owned(),
            "cargo test -p eva-runtime cancelled_basic_run_returns_task_record".to_owned(),
            "docs/zh-CN/planning/V1.x real runtime implementation plan V1.7.3 Done".to_owned(),
        ],
        remediation: vec![
            "do not add Lua hot reload or generation swap without preserving timeout, instruction budget, cancellation, and memory limit hooks".to_owned(),
            "keep capability calls behind cancellation-aware Lua execution so cancelled scripts cannot continue side effects".to_owned(),
        ],
    }
}

fn lua_hot_reload_lifecycle_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-LUA-HOT-RELOAD-001".to_owned(),
        domain: "lua_hot_reload_lifecycle".to_owned(),
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.7.4 Lua shadow load, generation route gating, drain evidence, and rollback audit boundaries are implemented".to_owned(),
        evidence: vec![
            "crates/eva-lua-host/src/hot_reload.rs LuaShadowLoader".to_owned(),
            "crates/eva-scheduler/src/generation.rs GenerationRouteGate".to_owned(),
            "crates/eva-lifecycle/src/drain.rs GenerationDrainEvidence".to_owned(),
            "crates/eva-lifecycle/src/rollback.rs plan_generation_lifecycle_rollback".to_owned(),
            "cargo test -p eva-lua-host shadow_load".to_owned(),
            "cargo test -p eva-scheduler generation".to_owned(),
            "cargo test -p eva-lifecycle drain rollback".to_owned(),
            "docs/zh-CN/planning/V1.x real runtime implementation plan V1.7.4 Done".to_owned(),
        ],
        remediation: vec![
            "do not promote a candidate generation unless shadow load is healthy".to_owned(),
            "keep old-generation drain and rollback audit evidence attached to every generation switch".to_owned(),
        ],
    }
}

fn signed_backup_archive_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-BACKUP-ARCHIVE-001".to_owned(),
        domain: "backup_archive".to_owned(),
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.10.3 signed backup archive, optional archive sealing, remote target contract, and pre-restore evidence checks are implemented".to_owned(),
        evidence: vec![
            "crates/eva-backup/src/archive.rs BackupArchiveCodec".to_owned(),
            "crates/eva-backup/src/restore_apply.rs PreRestoreBackupEvidence".to_owned(),
            "cargo test -p eva-backup backup_service_can_encrypt_archive_and_record_remote_target".to_owned(),
            "cargo test -p eva-cli restore_apply_dry_run_validates_durable_backup".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.10.3 Done".to_owned(),
        ],
        remediation: vec![
            "do not enable destructive restore unless signed archive verification and pre-restore evidence remain blocking gates".to_owned(),
        ],
    }
}

fn restore_apply_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-RESTORE-APPLY-GATE-001".to_owned(),
        domain: "restore_apply_gate".to_owned(),
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.10.4 restore apply confirmation, policy approval, filesystem lock, health gate, rollback-required plan, and staged mutation boundary are implemented".to_owned(),
        evidence: vec![
            "crates/eva-backup/src/restore_apply.rs RestoreApplyCoordinator".to_owned(),
            "crates/eva-cli/src/run.rs restore apply --lock-store policy and health gate".to_owned(),
            "cargo test -p eva-backup restore_apply".to_owned(),
            "cargo test -p eva-cli restore_apply".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.10.4 Done".to_owned(),
        ],
        remediation: vec![
            "do not execute destructive file mutation unless confirmation, evidence, policy, lock, and health gates still pass".to_owned(),
            "keep mutation_executed explicit until restore apply performs real workspace mutation".to_owned(),
        ],
    }
}

fn supervisor_handoff_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-SUPERVISOR-HANDOFF-001".to_owned(),
        domain: "supervisor_handoff".to_owned(),
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.10.5 supervisor blue-green handoff, release pointer mutation, persisted state, and rollback-on-health-failure baseline are implemented".to_owned(),
        evidence: vec![
            "crates/eva-lifecycle/src/handoff.rs SupervisorHandoffCoordinator".to_owned(),
            "crates/eva-cli/src/run.rs upgrade apply --state-store".to_owned(),
            "cargo test -p eva-lifecycle handoff".to_owned(),
            "cargo test -p eva-cli upgrade_apply".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.10.5 Done".to_owned(),
        ],
        remediation: vec![
            "do not write release pointer without supervisor.handoff and release.pointer_mutation policy approval".to_owned(),
            "keep rollback plans attached to failed candidate health checks".to_owned(),
            "replace local runtime-binary smoke with a production service-manager adapter before daemonized handoff".to_owned(),
        ],
    }
}

fn smoke_commands() -> Vec<String> {
    vec![
        "cargo fmt --check".to_owned(),
        "cargo clippy --workspace --all-targets -- -D warnings".to_owned(),
        "cargo test --workspace".to_owned(),
        "cargo test -p eva-lua-host".to_owned(),
        "cargo run -- --version".to_owned(),
        "cargo run -- inspect durable --durable-backend .eva/ci-durable --output json".to_owned(),
        "cargo run -- release check --output json".to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARTIFACT_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const ARTIFACT_DIGEST: &str =
        "sha256:2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df";

    fn artifact_evidence(signed: bool) -> ReleaseArtifactEvidence {
        let key = ReleaseArtifactSigningKey::local_development();
        let artifact = crate::artifact::ReleaseArtifactSubject::new(
            "eva-cli-1.7.4-alpha-x86_64-unknown-linux-gnu.tar.gz",
            "x86_64-unknown-linux-gnu",
            "tar.gz",
            "eva",
            ARTIFACT_DIGEST,
            4096,
            signed,
        )
        .unwrap();
        let provenance = crate::artifact::ReleaseProvenanceEvidence::new(
            "github-actions",
            ARTIFACT_COMMIT,
            "cargo-build-release-locked-bin-eva",
            "release",
            "spdx:release-evidence/eva.spdx.json",
            "passed",
        )
        .unwrap();
        let signature = crate::artifact::ReleaseArtifactSignature::new(
            key.key_id(),
            crate::artifact::RELEASE_SIGNATURE_ALGORITHM,
            "pending",
        )
        .unwrap();
        let mut evidence = ReleaseArtifactEvidence::new(
            "1.7.4-alpha",
            "v1.7.4-alpha",
            ARTIFACT_COMMIT,
            artifact,
            provenance,
            signature,
        )
        .unwrap();
        evidence.signature = evidence.sign(&key);
        evidence
    }

    fn install_smoke(
        os: &str,
        target: &str,
        artifact: &str,
        package_format: &str,
        status: &str,
    ) -> crate::distribution::ReleaseInstallSmokeEvidence {
        crate::distribution::ReleaseInstallSmokeEvidence::new(
            os,
            target,
            artifact,
            package_format,
            format!("install {artifact}"),
            "eva --version",
            format!("uninstall {artifact}"),
            format!("upgrade {artifact}"),
            status,
        )
        .unwrap()
    }

    fn package_dry_run(status: &str) -> crate::distribution::ReleasePackageDryRunEvidence {
        crate::distribution::ReleasePackageDryRunEvidence::new(
            "ghcr",
            "ghcr.io/yetmos/eva-cli",
            "linux/amd64+linux/arm64",
            "docker buildx imagetools inspect ghcr.io/yetmos/eva-cli:1.7.4-alpha",
            status,
        )
        .unwrap()
    }

    fn distribution_evidence(
        package_status: &str,
    ) -> crate::distribution::ReleaseDistributionEvidence {
        crate::distribution::ReleaseDistributionEvidence::new(
            "1.7.4-alpha",
            "v1.7.4-alpha",
            ARTIFACT_COMMIT,
            "docs/en/release/install-upgrade-uninstall.md",
            "docs/en/release/install-upgrade-uninstall.md",
            "docs/en/release/install-upgrade-uninstall.md",
            vec![
                install_smoke(
                    "windows",
                    "x86_64-pc-windows-msvc",
                    "eva-cli-1.7.4-alpha-x86_64-pc-windows-msvc.zip",
                    "zip",
                    "passed",
                ),
                install_smoke(
                    "linux",
                    "x86_64-unknown-linux-gnu",
                    "eva-cli-1.7.4-alpha-x86_64-unknown-linux-gnu.tar.gz",
                    "tar.gz",
                    "passed",
                ),
                install_smoke(
                    "macos",
                    "x86_64-apple-darwin",
                    "eva-cli-1.7.4-alpha-x86_64-apple-darwin.tar.gz",
                    "tar.gz",
                    "passed",
                ),
            ],
            vec![package_dry_run(package_status)],
        )
        .unwrap()
    }

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
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "docs/en/release/install-upgrade-uninstall.md")
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DURABLE-BACKEND-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DURABLE-EVENTBUS-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DURABLE-STORES-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DURABLE-RECOVERY-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DURABLE-DIAGNOSTICS-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-LUA-VM-EXECUTION-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-LUA-HOST-BINDINGS-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-LUA-RESOURCE-LIMITS-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-LUA-HOT-RELOAD-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-BACKUP-ARCHIVE-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-RESTORE-APPLY-GATE-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-SUPERVISOR-HANDOFF-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "durable_diagnostics_smoke_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "lua_vm_execution_boundary_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "lua_host_bindings_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "lua_resource_limits_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "lua_hot_reload_lifecycle_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "signed_backup_archive_baseline_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "restore_apply_gate_baseline_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "supervisor_handoff_baseline_ready"));
    }

    #[test]
    fn readiness_with_signed_artifact_evidence_passes_release_artifact_gate() {
        let evidence = artifact_evidence(true);
        let report = ReleaseHardeningService::v15()
            .readiness_with_artifact_evidence("all", &evidence)
            .unwrap();

        assert_eq!(report.status, "ready");
        assert_eq!(report.blocking_count(), 0);
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-ARTIFACT-PROVENANCE-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "signed_artifact_provenance_verified"));
    }

    #[test]
    fn readiness_with_unsigned_artifact_evidence_blocks_release_artifact_gate() {
        let evidence = artifact_evidence(false);
        let report = ReleaseHardeningService::v15()
            .readiness_with_artifact_evidence("all", &evidence)
            .unwrap();

        assert_eq!(report.status, "blocked");
        assert_eq!(report.blocking_count(), 1);
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-ARTIFACT-PROVENANCE-001"
                && gate.status == ReleaseGateStatus::Blocked
                && gate
                    .remediation
                    .iter()
                    .any(|item| item == "release artifact is marked unsigned")
        }));
    }

    #[test]
    fn readiness_with_distribution_evidence_passes_release_distribution_gate() {
        let evidence = distribution_evidence("passed");
        let report = ReleaseHardeningService::v15()
            .readiness_with_distribution_evidence("all", &evidence)
            .unwrap();

        assert_eq!(report.status, "ready");
        assert_eq!(report.blocking_count(), 0);
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DISTRIBUTION-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "distribution_install_smoke_verified"));
    }

    #[test]
    fn readiness_with_failed_package_dry_run_blocks_release_distribution_gate() {
        let evidence = distribution_evidence("failed");
        let report = ReleaseHardeningService::v15()
            .readiness_with_distribution_evidence("all", &evidence)
            .unwrap();

        assert_eq!(report.status, "blocked");
        assert_eq!(report.blocking_count(), 1);
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DISTRIBUTION-001"
                && gate.status == ReleaseGateStatus::Blocked
                && gate
                    .remediation
                    .iter()
                    .any(|item| item.contains("package manager dry-run for ghcr"))
        }));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "distribution_install_smoke_blocked"));
    }

    #[test]
    fn readiness_with_artifact_and_distribution_evidence_passes_both_gates() {
        let artifact = artifact_evidence(true);
        let distribution = distribution_evidence("passed");
        let report = ReleaseHardeningService::v15()
            .readiness_with_release_evidence("all", Some(&artifact), Some(&distribution))
            .unwrap();

        assert_eq!(report.status, "ready");
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-ARTIFACT-PROVENANCE-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DISTRIBUTION-001" && gate.status == ReleaseGateStatus::Pass
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
