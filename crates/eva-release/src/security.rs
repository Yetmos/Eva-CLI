//! Security review findings for release hardening.

use crate::checklist::ReleaseGateStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecuritySeverity {
    Low,
    Medium,
    High,
    Critical,
}

impl SecuritySeverity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    pub const fn is_required_gate(self) -> bool {
        matches!(self, Self::High | Self::Critical)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityFinding {
    pub id: String,
    pub boundary: String,
    pub severity: SecuritySeverity,
    pub status: ReleaseGateStatus,
    pub summary: String,
    pub evidence: Vec<String>,
    pub remediation: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityReviewReport {
    pub version: String,
    pub status: String,
    pub findings: Vec<SecurityFinding>,
    pub audit: Vec<String>,
}

impl SecurityFinding {
    pub fn passed(
        id: impl Into<String>,
        boundary: impl Into<String>,
        severity: SecuritySeverity,
        summary: impl Into<String>,
        evidence: Vec<String>,
    ) -> Self {
        Self {
            id: id.into(),
            boundary: boundary.into(),
            severity,
            status: ReleaseGateStatus::Pass,
            summary: summary.into(),
            evidence,
            remediation: Vec::new(),
        }
    }

    pub fn tracked(
        id: impl Into<String>,
        boundary: impl Into<String>,
        severity: SecuritySeverity,
        summary: impl Into<String>,
        evidence: Vec<String>,
        remediation: Vec<String>,
    ) -> Self {
        Self {
            id: id.into(),
            boundary: boundary.into(),
            severity,
            status: ReleaseGateStatus::Warn,
            summary: summary.into(),
            evidence,
            remediation,
        }
    }
}

impl SecurityReviewReport {
    pub fn blocking_findings(&self) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.severity.is_required_gate() && finding.status.is_blocking())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use crate::ReleaseHardeningService;

    #[test]
    fn review_covers_required_boundaries() {
        let report = ReleaseHardeningService::v15().security_review();
        let boundaries: Vec<_> = report
            .findings
            .iter()
            .map(|finding| finding.boundary.as_str())
            .collect();

        assert!(boundaries.contains(&"policy"));
        assert!(boundaries.contains(&"lua_sandbox"));
        assert!(boundaries.contains(&"secret_redaction"));
        assert!(boundaries.contains(&"mcp"));
        assert!(boundaries.contains(&"hardware"));
        assert_eq!(report.blocking_findings(), 0);
    }
}
