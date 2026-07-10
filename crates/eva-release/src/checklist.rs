//! Release readiness checklist aggregation.

use crate::artifact::{
    ReleaseArtifactEvidence, ReleaseArtifactSigningKey, ReleaseArtifactVerificationReport,
};
use crate::benchmark::{ReleaseBenchmarkEvidence, ReleaseBenchmarkVerificationReport};
use crate::distribution::{ReleaseDistributionEvidence, ReleaseDistributionVerificationReport};
use crate::migration::{CompatibilityPolicy, MigrationGuide, MigrationStep};
use crate::performance::{PerformanceBaselineReport, PerformanceBudget};
use crate::scanner::{ReleaseSecurityScanEvidence, ReleaseSecurityScanVerificationReport};
use crate::security::{SecurityFinding, SecurityReviewReport, SecuritySeverity};
use eva_core::EvaError;
use eva_mcp::{McpCompatibilityMatrix, McpCompatibilityReport};

const CURRENT_RELEASE_VERSION: &str = "1.11.5-alpha";
const CURRENT_RELEASE_LABEL: &str = "V1.11.5-alpha";

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
        self.readiness_inner(target.into(), None, None, None, None)
    }

    pub fn readiness_with_artifact_evidence(
        &self,
        target: impl Into<String>,
        evidence: &ReleaseArtifactEvidence,
    ) -> Result<ReleaseReadinessReport, EvaError> {
        self.readiness_inner(target.into(), Some(evidence), None, None, None)
    }

    pub fn readiness_with_distribution_evidence(
        &self,
        target: impl Into<String>,
        evidence: &ReleaseDistributionEvidence,
    ) -> Result<ReleaseReadinessReport, EvaError> {
        self.readiness_inner(target.into(), None, Some(evidence), None, None)
    }

    pub fn readiness_with_security_scan_evidence(
        &self,
        target: impl Into<String>,
        evidence: &ReleaseSecurityScanEvidence,
    ) -> Result<ReleaseReadinessReport, EvaError> {
        self.readiness_inner(target.into(), None, None, Some(evidence), None)
    }

    pub fn readiness_with_benchmark_evidence(
        &self,
        target: impl Into<String>,
        evidence: &ReleaseBenchmarkEvidence,
    ) -> Result<ReleaseReadinessReport, EvaError> {
        self.readiness_inner(target.into(), None, None, None, Some(evidence))
    }

    pub fn readiness_with_release_evidence(
        &self,
        target: impl Into<String>,
        artifact_evidence: Option<&ReleaseArtifactEvidence>,
        distribution_evidence: Option<&ReleaseDistributionEvidence>,
        security_scan_evidence: Option<&ReleaseSecurityScanEvidence>,
        benchmark_evidence: Option<&ReleaseBenchmarkEvidence>,
    ) -> Result<ReleaseReadinessReport, EvaError> {
        self.readiness_inner(
            target.into(),
            artifact_evidence,
            distribution_evidence,
            security_scan_evidence,
            benchmark_evidence,
        )
    }

    fn readiness_inner(
        &self,
        target: String,
        artifact_evidence: Option<&ReleaseArtifactEvidence>,
        distribution_evidence: Option<&ReleaseDistributionEvidence>,
        security_scan_evidence: Option<&ReleaseSecurityScanEvidence>,
        benchmark_evidence: Option<&ReleaseBenchmarkEvidence>,
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
        gates.push(service_manager_abstraction_gate());
        gates.push(daemon_runtime_gate());
        let mcp_compatibility_report = McpCompatibilityMatrix::v1137_fixture().verify()?;
        gates.push(mcp_compatibility_matrix_gate(Some(
            &mcp_compatibility_report,
        )));
        gates.push(provider_supervision_gate());
        gates.push(hardware_safety_release_gate());
        gates.push(public_json_contract_gate());
        let artifact_report = artifact_evidence
            .map(|evidence| evidence.verify(&ReleaseArtifactSigningKey::local_development()));
        if let Some(report) = artifact_report.as_ref() {
            gates.push(release_artifact_provenance_gate(report));
        }
        let distribution_report = distribution_evidence.map(ReleaseDistributionEvidence::verify);
        if let Some(report) = distribution_report.as_ref() {
            gates.push(release_distribution_gate(report));
        }
        let security_scan_report = security_scan_evidence.map(ReleaseSecurityScanEvidence::verify);
        if let Some(report) = security_scan_report.as_ref() {
            gates.push(release_security_scan_gate(report));
        }
        let benchmark_report = benchmark_evidence.map(ReleaseBenchmarkEvidence::verify);
        if let Some(report) = benchmark_report.as_ref() {
            gates.push(release_benchmark_gate(report));
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
            audit: release_audit(
                artifact_report.as_ref(),
                distribution_report.as_ref(),
                security_scan_report.as_ref(),
                benchmark_report.as_ref(),
            ),
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
                        "V1.15.1 hardware bind reports platform OS permission evidence with remediation and raw_device_path_exposed=false".to_owned(),
                        "PlatformOsPermissionProvider blocks driver start before lease claim when permission is missing".to_owned(),
                        "V1.15.4 daemon hotplug subscriber publishes typed logical-state events and reports raw_handles_exposed=false".to_owned(),
                        "hotplug watcher crash path releases active hardware leases".to_owned(),
                        "V1.15.5 release check records simulator parity, permission denial, lease cleanup, and hotplug smoke evidence".to_owned(),
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
                        "restore apply keeps no-step plans mutation_executed=false and reports mutation_executed=true only after staged file mutation commits".to_owned(),
                        "restore rollback apply verifies pre-restore archive entries and transaction log before reverse mutation".to_owned(),
                        "restore apply and rollback expose operator confirmation with confirm token, target root, affected count, state flags, and irreversible warning".to_owned(),
                        "upgrade apply can commit a controlled supervisor handoff and release pointer mutation inside the configured state store after policy approval".to_owned(),
                    ],
                    vec![
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
                "migration:v1.5.1_to_v1.11.5-alpha:no_breaking_changes".to_owned(),
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
                    "docs/en/release/release-notes-v1.11.5.md".to_owned(),
                ],
                remediation: vec!["update docs and i18n validation before tagging release".to_owned()],
            },
        ]
    }
}

fn release_audit(
    artifact_report: Option<&ReleaseArtifactVerificationReport>,
    distribution_report: Option<&ReleaseDistributionVerificationReport>,
    security_scan_report: Option<&ReleaseSecurityScanVerificationReport>,
    benchmark_report: Option<&ReleaseBenchmarkVerificationReport>,
) -> Vec<String> {
    let mut audit = vec![
        "release:readiness:v1.11.5-alpha".to_owned(),
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
        "service_manager_abstraction_ready".to_owned(),
        "daemon_runtime_readiness_gate_ready".to_owned(),
        "mcp_compatibility_matrix_ready".to_owned(),
        "provider_supervision_readiness_gate_ready".to_owned(),
        "hardware_safety_release_gate_ready".to_owned(),
        "public_json_contract_diff_ready".to_owned(),
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
    if let Some(report) = security_scan_report {
        if report.status == "verified" {
            audit.push("external_security_scan_verified".to_owned());
        } else {
            audit.push("external_security_scan_blocked".to_owned());
        }
        audit.extend(report.audit.iter().cloned());
    }
    if let Some(report) = benchmark_report {
        if report.status == "verified" {
            audit.push("production_benchmark_verified".to_owned());
        } else {
            audit.push("production_benchmark_blocked".to_owned());
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

fn release_security_scan_gate(report: &ReleaseSecurityScanVerificationReport) -> ReleaseGate {
    let mut evidence = vec![
        format!("scanner:{}", report.scanner),
        format!("scanner_version:{}", report.scanner_version),
        format!("scan_status:{}", report.scan_status),
        format!("source_commit:{}", report.source_commit),
        format!("finding_count:{}", report.findings.len()),
        format!("blocking_findings:{}", report.blocking_findings.len()),
    ];
    evidence.extend(report.findings.iter().map(|finding| {
        format!(
            "finding:{}:{}:{}",
            finding.id,
            finding.package,
            finding.severity.as_str()
        )
    }));
    evidence.extend(report.audit.iter().cloned());
    ReleaseGate {
        id: "REL-SECURITY-SCAN-001".to_owned(),
        domain: "external_security_scan".to_owned(),
        status: if report.status == "verified" {
            ReleaseGateStatus::Pass
        } else {
            ReleaseGateStatus::Blocked
        },
        required: true,
        summary: "V1.11.3 external security scanner evidence has no high or critical findings"
            .to_owned(),
        evidence,
        remediation: if report.risks.is_empty() {
            Vec::new()
        } else {
            report.risks.clone()
        },
    }
}

fn release_benchmark_gate(report: &ReleaseBenchmarkVerificationReport) -> ReleaseGate {
    let mut evidence = vec![
        format!("benchmark_status:{}", report.benchmark_status),
        format!("source_commit:{}", report.source_commit),
        format!("measurement_count:{}", report.measurements.len()),
        format!("regression_count:{}", report.regressions.len()),
    ];
    evidence.extend(report.measurements.iter().map(|measurement| {
        format!(
            "measurement:{}:{}ms/{}ms:{}samples",
            measurement.component,
            measurement.observed_ms,
            measurement.budget_ms,
            measurement.sample_count
        )
    }));
    evidence.extend(report.audit.iter().cloned());
    ReleaseGate {
        id: "REL-BENCHMARK-001".to_owned(),
        domain: "production_benchmark".to_owned(),
        status: if report.status == "verified" {
            ReleaseGateStatus::Pass
        } else {
            ReleaseGateStatus::Blocked
        },
        required: true,
        summary: "V1.11.3 production benchmark evidence stays within configured budgets".to_owned(),
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
        summary: "V1.7.4 Lua shadow load, generation route gating, drain evidence, and rollback audit boundaries remain implemented".to_owned(),
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
        summary: "V1.10.4/V1.14.4 restore apply confirmation, policy approval, filesystem lock, health gate, staged file mutation, rollback-required transaction evidence, rollback apply, and operator confirmation are implemented".to_owned(),
        evidence: vec![
            "crates/eva-backup/src/restore_apply.rs RestoreApplyCoordinator, RestoreMutationEngine, and RestoreRollbackEngine".to_owned(),
            "crates/eva-cli/src/run.rs restore apply/rollback --lock-store policy, health, and mutation gate".to_owned(),
            "cargo test -p eva-backup restore_apply".to_owned(),
            "cargo test -p eva-cli restore_apply".to_owned(),
            "cargo test -p eva-cli restore_rollback".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.14.4 Done".to_owned(),
        ],
        remediation: vec![
            "do not execute restore rollback apply unless confirmation, evidence, policy, rollback lock, health, staged plan, transaction log, and pre-restore archive entries still pass".to_owned(),
            "keep mutation_executed explicit: false for no-step plans, true only after staged mutation commits".to_owned(),
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

fn service_manager_abstraction_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-SERVICE-MANAGER-ABSTRACTION-001".to_owned(),
        domain: "service_manager_abstraction".to_owned(),
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.14.5 OS service-manager adapter trait, typed config, fake handoff, and rollback evidence are implemented".to_owned(),
        evidence: vec![
            "crates/eva-lifecycle/src/service_manager.rs ServiceManagerAdapter and FakeServiceManagerAdapter".to_owned(),
            "crates/eva-config/src/eva_yaml.rs service_manager typed config".to_owned(),
            "cargo test -p eva-lifecycle service_manager".to_owned(),
            "cargo test -p eva-config service_manager".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.14.5 Done".to_owned(),
        ],
        remediation: vec![
            "keep fake adapter limited to local tests and explicit fake config".to_owned(),
            "do not claim Windows Service, systemd, or launchd handoff until V1.14.6 platform adapters pass controlled tests".to_owned(),
        ],
    }
}

fn daemon_runtime_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-DAEMON-RUNTIME-001".to_owned(),
        domain: "daemon_runtime".to_owned(),
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.12 daemon process boundary, filesystem mailbox control, durable task lifecycle, scheduler retry tick, and daemon-backed agent drain/reload mutation readiness are implemented".to_owned(),
        evidence: vec![
            "crates/eva-runtime/src/daemon.rs start_daemon and run_control_loop".to_owned(),
            "crates/eva-runtime/src/scheduler_retry.rs run_scheduler_retry_tick".to_owned(),
            "crates/eva-cli/src/run/daemon_cmd.rs daemon start/status/shutdown/submit/cancel/drain/reload".to_owned(),
            "crates/eva-cli/src/run/agent_cmd.rs daemon-backed agent drain/reload fallback".to_owned(),
            "cargo test -p eva-runtime daemon_control_status_and_shutdown_round_trip_has_trace_id".to_owned(),
            "cargo test -p eva-runtime daemon_control_loop_ticks_scheduler_retry_once".to_owned(),
            "cargo test -p eva-runtime daemon_drain_mutates_agent_control_state".to_owned(),
            "cargo test -p eva-runtime daemon_reload_mutates_generation_route_state".to_owned(),
            "cargo test -p eva-cli daemon_control_status_and_shutdown_round_trip_via_cli".to_owned(),
            "cargo test -p eva-cli agent_drain_and_reload_use_daemon_mutation_when_available".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.12.6 Done".to_owned(),
        ],
        remediation: vec![
            "keep daemon readiness limited to local foreground/filesystem control until OS service-manager adapters exist".to_owned(),
            "do not claim production hot reload or provider supervision without provider execution-state recovery tests".to_owned(),
        ],
    }
}

fn mcp_compatibility_matrix_gate(report: Option<&McpCompatibilityReport>) -> ReleaseGate {
    match report {
        Some(report) => {
            let status = if report.status == "compatible" {
                ReleaseGateStatus::Pass
            } else {
                ReleaseGateStatus::Blocked
            };
            let mut evidence = report.audit.clone();
            evidence.extend(
                report
                    .failures
                    .iter()
                    .map(|failure| format!("mcp.compat.failure:{failure}")),
            );
            ReleaseGate {
                id: "REL-MCP-COMPAT-001".to_owned(),
                domain: "mcp_compatibility".to_owned(),
                status,
                required: true,
                summary: "MCP transport, schema, stream lifecycle, and server-surface compatibility matrix is present".to_owned(),
                evidence,
                remediation: if status.is_blocking() {
                    vec![
                        "provide a passing MCP compatibility matrix before release".to_owned(),
                        "cover stdio/http JSON-RPC, stream abort/cleanup, schema support, and explicit-tool server gate".to_owned(),
                    ]
                } else {
                    Vec::new()
                },
            }
        }
        None => ReleaseGate {
            id: "REL-MCP-COMPAT-001".to_owned(),
            domain: "mcp_compatibility".to_owned(),
            status: ReleaseGateStatus::Blocked,
            required: true,
            summary: "MCP compatibility matrix is required before release".to_owned(),
            evidence: vec!["mcp.compatibility_matrix:missing".to_owned()],
            remediation: vec![
                "generate or attach an MCP compatibility matrix fixture".to_owned(),
                "verify stream abort/cleanup and explicit-tool server gate evidence".to_owned(),
            ],
        },
    }
}

fn provider_supervision_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-PROVIDER-SUPERVISION-001".to_owned(),
        domain: "provider_supervision".to_owned(),
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "Provider stdio/http/MCP/Skill supervision, credential scope, admission limits, stream artifacts, recovery, and MCP compatibility gates are present".to_owned(),
        evidence: vec![
            "provider.supervisor_slot:stdio,http,mcp,skill".to_owned(),
            "provider.process_table:in_memory_snapshot_and_durable_mirror".to_owned(),
            "provider.credential_session:scoped_env_header_token_redaction".to_owned(),
            "provider.admission:concurrency_rate_circuit_backoff".to_owned(),
            "provider.stream_artifact:bounded_redacted_summary".to_owned(),
            "provider.recovery:daemon_restart_interrupted_backoff_evidence".to_owned(),
            "provider.mcp_compatibility_gate:REL-MCP-COMPAT-001".to_owned(),
            "provider.os_process_supervisor:not_claimed".to_owned(),
            "cargo test -p eva-adapter supervisor runtime::tests stream::tests".to_owned(),
            "cargo test -p eva-runtime recovery".to_owned(),
            "cargo test -p eva-storage provider_process".to_owned(),
            "docs/zh-CN/planning/V1.x真实运行时能力补齐实施计划.md V1.13.8 Done".to_owned(),
        ],
        remediation: Vec::new(),
    }
}

fn hardware_safety_release_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-HARDWARE-SAFETY-001".to_owned(),
        domain: "hardware_safety".to_owned(),
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "V1.15.5 hardware safety release gate records simulator parity, permission denial, lease cleanup, and hotplug smoke evidence for alpha".to_owned(),
        evidence: vec![
            "hardware.safety.release_mode:alpha_simulator_only".to_owned(),
            "simulator_parity:run_simulator_contract_suite rejects raw I/O and capability bypass".to_owned(),
            "permission_denial:PlatformOsPermissionProvider reports remediation and raw_device_path_exposed=false before lease claim".to_owned(),
            "lease_cleanup:driver crash and hotplug watcher crash release active hardware leases".to_owned(),
            "hotplug_smoke:daemon manifest snapshot subscriber publishes typed events, persists hardware-hotplug.state, and reports raw_handles_exposed=false".to_owned(),
            "real_hardware_fixture:not_required_for_alpha".to_owned(),
            "cargo test -p eva-hardware simulator_contract_suite_rejects_raw_io_and_capability_bypass".to_owned(),
            "cargo test -p eva-hardware permission".to_owned(),
            "cargo test -p eva-hardware hotplug".to_owned(),
            "cargo test -p eva-runtime daemon_hotplug_subscriber_persists_state_across_restart".to_owned(),
            "cargo test -p eva-cli daemon_start_foreground_smoke_reports_verified_boundaries".to_owned(),
        ],
        remediation: vec![
            "production releases must attach real or virtual hardware fixture evidence before claiming real USB/serial/BLE/socket/vendor SDK I/O".to_owned(),
            "do not remove permission denial, lease cleanup, raw handle suppression, or hotplug smoke checks when replacing simulator-only alpha evidence".to_owned(),
        ],
    }
}

fn public_json_contract_gate() -> ReleaseGate {
    ReleaseGate {
        id: "REL-JSON-CONTRACT-001".to_owned(),
        domain: "cli_json_contract".to_owned(),
        status: ReleaseGateStatus::Pass,
        required: true,
        summary: "Public CLI JSON envelope and command data contracts are protected by additive-compatible golden subset diffs".to_owned(),
        evidence: vec![
            "scripts/validate-cli-json-contracts.ps1".to_owned(),
            "contracts/cli-json/version.json".to_owned(),
            "contracts/cli-json/run-basic.json".to_owned(),
            "contracts/cli-json/capability-list.json".to_owned(),
            "contracts/cli-json/hardware-bind.json".to_owned(),
            "contracts/cli-json/restore-plan.json".to_owned(),
            "contracts/cli-json/upgrade-check.json".to_owned(),
            "contracts/cli-json/release-check.json".to_owned(),
            "contract.diff:golden_subset_allows_additive_fields".to_owned(),
            "contract.diff:missing_or_renamed_fields_block".to_owned(),
        ],
        remediation: vec![
            "run ./scripts/validate-cli-json-contracts.ps1 before release".to_owned(),
            "treat removed or renamed JSON fields as breaking unless a new compatibility window is documented".to_owned(),
        ],
    }
}

fn smoke_commands() -> Vec<String> {
    vec![
        "cargo fmt --check".to_owned(),
        "cargo clippy --workspace --all-targets -- -D warnings".to_owned(),
        "cargo test --workspace".to_owned(),
        "cargo test -p eva-lua-host".to_owned(),
        "./scripts/validate-cli-json-contracts.ps1".to_owned(),
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
            "eva-cli-1.11.5-alpha-x86_64-unknown-linux-gnu.tar.gz",
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
            "1.11.5-alpha",
            "v1.11.5-alpha",
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
            "docker buildx imagetools inspect ghcr.io/yetmos/eva-cli:1.11.5-alpha",
            status,
        )
        .unwrap()
    }

    fn distribution_evidence(
        package_status: &str,
    ) -> crate::distribution::ReleaseDistributionEvidence {
        crate::distribution::ReleaseDistributionEvidence::new(
            "1.11.5-alpha",
            "v1.11.5-alpha",
            ARTIFACT_COMMIT,
            "docs/en/release/install-upgrade-uninstall.md",
            "docs/en/release/install-upgrade-uninstall.md",
            "docs/en/release/install-upgrade-uninstall.md",
            vec![
                install_smoke(
                    "windows",
                    "x86_64-pc-windows-msvc",
                    "eva-cli-1.11.5-alpha-x86_64-pc-windows-msvc.zip",
                    "zip",
                    "passed",
                ),
                install_smoke(
                    "linux",
                    "x86_64-unknown-linux-gnu",
                    "eva-cli-1.11.5-alpha-x86_64-unknown-linux-gnu.tar.gz",
                    "tar.gz",
                    "passed",
                ),
                install_smoke(
                    "macos",
                    "x86_64-apple-darwin",
                    "eva-cli-1.11.5-alpha-x86_64-apple-darwin.tar.gz",
                    "tar.gz",
                    "passed",
                ),
            ],
            vec![package_dry_run(package_status)],
        )
        .unwrap()
    }

    fn security_scan_finding(severity: &str) -> crate::scanner::ReleaseSecurityScanFinding {
        crate::scanner::ReleaseSecurityScanFinding::new(
            "RUSTSEC-0000-0000",
            "demo-crate",
            "1.0.0",
            severity,
            "demo advisory",
            "upgrade demo-crate",
        )
        .unwrap()
    }

    fn security_scan_evidence(
        status: &str,
        findings: Vec<crate::scanner::ReleaseSecurityScanFinding>,
    ) -> crate::scanner::ReleaseSecurityScanEvidence {
        crate::scanner::ReleaseSecurityScanEvidence::new(
            "1.11.5-alpha",
            "v1.11.5-alpha",
            ARTIFACT_COMMIT,
            "cargo-audit",
            "1.0.0",
            status,
            "cargo audit --json",
            findings,
        )
        .unwrap()
    }

    fn benchmark_measurement(observed_ms: u64) -> crate::benchmark::ReleaseBenchmarkMeasurement {
        crate::benchmark::ReleaseBenchmarkMeasurement::new(
            "release.check",
            "cli release check wall time",
            200,
            observed_ms,
            3,
            "target/release/eva release check --output json",
            "github-actions-ubuntu-latest",
        )
        .unwrap()
    }

    fn benchmark_evidence(
        status: &str,
        observed_ms: u64,
    ) -> crate::benchmark::ReleaseBenchmarkEvidence {
        crate::benchmark::ReleaseBenchmarkEvidence::new(
            "1.11.5-alpha",
            "v1.11.5-alpha",
            ARTIFACT_COMMIT,
            status,
            vec![benchmark_measurement(observed_ms)],
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
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-SERVICE-MANAGER-ABSTRACTION-001"
                && gate.status == ReleaseGateStatus::Pass
                && gate.domain == "service_manager_abstraction"
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-DAEMON-RUNTIME-001"
                && gate.status == ReleaseGateStatus::Pass
                && gate.domain == "daemon_runtime"
                && gate
                    .evidence
                    .iter()
                    .any(|item| item.contains("daemon_control_loop_ticks_scheduler_retry_once"))
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-MCP-COMPAT-001"
                && gate.status == ReleaseGateStatus::Pass
                && gate.domain == "mcp_compatibility"
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "mcp.transport_count:2")
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-PROVIDER-SUPERVISION-001"
                && gate.status == ReleaseGateStatus::Pass
                && gate.domain == "provider_supervision"
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "provider.os_process_supervisor:not_claimed")
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-HARDWARE-SAFETY-001"
                && gate.status == ReleaseGateStatus::Pass
                && gate.domain == "hardware_safety"
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "hardware.safety.release_mode:alpha_simulator_only")
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "real_hardware_fixture:not_required_for_alpha")
        }));
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-JSON-CONTRACT-001"
                && gate.status == ReleaseGateStatus::Pass
                && gate.domain == "cli_json_contract"
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "scripts/validate-cli-json-contracts.ps1")
                && gate
                    .evidence
                    .iter()
                    .any(|item| item == "contracts/cli-json/release-check.json")
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
        assert!(report
            .audit
            .iter()
            .any(|item| item == "service_manager_abstraction_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "daemon_runtime_readiness_gate_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "mcp_compatibility_matrix_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "provider_supervision_readiness_gate_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "hardware_safety_release_gate_ready"));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "public_json_contract_diff_ready"));
    }

    #[test]
    fn mcp_compatibility_gate_blocks_missing_matrix() {
        let gate = mcp_compatibility_matrix_gate(None);

        assert_eq!(gate.id, "REL-MCP-COMPAT-001");
        assert_eq!(gate.status, ReleaseGateStatus::Blocked);
        assert!(gate.required);
        assert!(gate
            .evidence
            .contains(&"mcp.compatibility_matrix:missing".to_owned()));
    }

    #[test]
    fn provider_supervision_gate_records_current_boundaries() {
        let gate = provider_supervision_gate();

        assert_eq!(gate.id, "REL-PROVIDER-SUPERVISION-001");
        assert_eq!(gate.status, ReleaseGateStatus::Pass);
        assert!(gate.required);
        assert!(gate
            .evidence
            .contains(&"provider.mcp_compatibility_gate:REL-MCP-COMPAT-001".to_owned()));
        assert!(gate
            .evidence
            .contains(&"provider.os_process_supervisor:not_claimed".to_owned()));
    }

    #[test]
    fn hardware_safety_release_gate_accepts_alpha_simulator_only_evidence() {
        let gate = hardware_safety_release_gate();

        assert_eq!(gate.id, "REL-HARDWARE-SAFETY-001");
        assert_eq!(gate.status, ReleaseGateStatus::Pass);
        assert!(gate.required);
        assert_eq!(gate.domain, "hardware_safety");
        assert!(gate
            .evidence
            .contains(&"hardware.safety.release_mode:alpha_simulator_only".to_owned()));
        assert!(gate.evidence.iter().any(|item| {
            item.contains("run_simulator_contract_suite") && item.contains("raw I/O")
        }));
        assert!(gate.evidence.iter().any(|item| {
            item.contains("raw_device_path_exposed=false") && item.contains("before lease claim")
        }));
        assert!(gate.evidence.iter().any(|item| {
            item.contains("hotplug watcher crash")
                && item.contains("release active hardware leases")
        }));
        assert!(gate.evidence.iter().any(|item| {
            item.contains("raw_handles_exposed=false") && item.contains("hardware-hotplug.state")
        }));
        assert!(gate.remediation.iter().any(|item| {
            item.contains("real or virtual hardware fixture evidence")
                && item.contains("claiming real USB")
        }));
    }

    #[test]
    fn public_json_contract_gate_records_additive_diff_suite() {
        let gate = public_json_contract_gate();

        assert_eq!(gate.id, "REL-JSON-CONTRACT-001");
        assert_eq!(gate.status, ReleaseGateStatus::Pass);
        assert!(gate.required);
        assert_eq!(gate.domain, "cli_json_contract");
        assert!(gate
            .evidence
            .contains(&"scripts/validate-cli-json-contracts.ps1".to_owned()));
        assert!(gate
            .evidence
            .contains(&"contracts/cli-json/version.json".to_owned()));
        assert!(gate
            .evidence
            .contains(&"contracts/cli-json/release-check.json".to_owned()));
        assert!(gate
            .evidence
            .iter()
            .any(|item| { item == "contract.diff:golden_subset_allows_additive_fields" }));
        assert!(gate
            .evidence
            .iter()
            .any(|item| { item == "contract.diff:missing_or_renamed_fields_block" }));
        assert!(gate.remediation.iter().any(|item| {
            item.contains("removed or renamed JSON fields") && item.contains("breaking")
        }));
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
            .readiness_with_release_evidence(
                "all",
                Some(&artifact),
                Some(&distribution),
                None,
                None,
            )
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
    fn readiness_with_clean_security_scan_evidence_passes_gate() {
        let evidence = security_scan_evidence("passed", Vec::new());
        let report = ReleaseHardeningService::v15()
            .readiness_with_security_scan_evidence("all", &evidence)
            .unwrap();

        assert_eq!(report.status, "ready");
        assert_eq!(report.blocking_count(), 0);
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-SECURITY-SCAN-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "external_security_scan_verified"));
    }

    #[test]
    fn readiness_with_high_security_scan_finding_blocks_gate() {
        let evidence = security_scan_evidence("passed", vec![security_scan_finding("high")]);
        let report = ReleaseHardeningService::v15()
            .readiness_with_security_scan_evidence("all", &evidence)
            .unwrap();

        assert_eq!(report.status, "blocked");
        assert_eq!(report.blocking_count(), 1);
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-SECURITY-SCAN-001"
                && gate.status == ReleaseGateStatus::Blocked
                && gate.remediation.iter().any(|item| {
                    item == "security scanner finding RUSTSEC-0000-0000 is high severity"
                })
        }));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "external_security_scan_blocked"));
    }

    #[test]
    fn readiness_with_benchmark_evidence_passes_gate() {
        let evidence = benchmark_evidence("passed", 120);
        let report = ReleaseHardeningService::v15()
            .readiness_with_benchmark_evidence("all", &evidence)
            .unwrap();

        assert_eq!(report.status, "ready");
        assert_eq!(report.blocking_count(), 0);
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-BENCHMARK-001" && gate.status == ReleaseGateStatus::Pass
        }));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "production_benchmark_verified"));
    }

    #[test]
    fn readiness_with_benchmark_regression_blocks_gate() {
        let evidence = benchmark_evidence("passed", 250);
        let report = ReleaseHardeningService::v15()
            .readiness_with_benchmark_evidence("all", &evidence)
            .unwrap();

        assert_eq!(report.status, "blocked");
        assert_eq!(report.blocking_count(), 1);
        assert!(report.gates.iter().any(|gate| {
            gate.id == "REL-BENCHMARK-001"
                && gate.status == ReleaseGateStatus::Blocked
                && gate
                    .remediation
                    .iter()
                    .any(|item| item == "benchmark release.check observed 250ms over 200ms budget")
        }));
        assert!(report
            .audit
            .iter()
            .any(|item| item == "production_benchmark_blocked"));
    }

    #[test]
    fn readiness_with_all_release_evidence_passes_release_gates() {
        let artifact = artifact_evidence(true);
        let distribution = distribution_evidence("passed");
        let security_scan = security_scan_evidence("passed", Vec::new());
        let benchmark = benchmark_evidence("passed", 120);
        let report = ReleaseHardeningService::v15()
            .readiness_with_release_evidence(
                "all",
                Some(&artifact),
                Some(&distribution),
                Some(&security_scan),
                Some(&benchmark),
            )
            .unwrap();

        assert_eq!(report.status, "ready");
        for gate_id in [
            "REL-ARTIFACT-PROVENANCE-001",
            "REL-DISTRIBUTION-001",
            "REL-SECURITY-SCAN-001",
            "REL-BENCHMARK-001",
        ] {
            assert!(report
                .gates
                .iter()
                .any(|gate| { gate.id == gate_id && gate.status == ReleaseGateStatus::Pass }));
        }
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
