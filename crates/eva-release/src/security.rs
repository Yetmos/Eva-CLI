//! 发布加固使用的内置安全审查发现。
//! Security review findings for release hardening.

use crate::checklist::ReleaseGateStatus;

/// 安全发现的影响严重级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecuritySeverity {
    /// 影响有限，不构成强制发布门禁。
    Low,
    /// 需要跟踪但默认不构成强制发布门禁。
    Medium,
    /// 高影响发现，必须作为发布门禁处理。
    High,
    /// 可造成严重系统风险的最高级别发现。
    Critical,
}

impl SecuritySeverity {
    /// 返回用于报告和证据清单的稳定严重级别字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    /// 判断该严重级别是否属于强制发布门禁。
    pub const fn is_required_gate(self) -> bool {
        matches!(self, Self::High | Self::Critical)
    }
}

/// 一个安全边界的审查结论、证据与补救建议。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityFinding {
    /// 发现的稳定标识。
    pub id: String,
    /// 被审查的信任或执行边界。
    pub boundary: String,
    /// 潜在影响严重级别。
    pub severity: SecuritySeverity,
    /// 当前发布门禁状态。
    pub status: ReleaseGateStatus,
    /// 风险和当前控制措施摘要。
    pub summary: String,
    /// 支撑结论的测试、代码或运行时证据。
    pub evidence: Vec<String>,
    /// 未通过或持续跟踪项的补救步骤。
    pub remediation: Vec<String>,
}

/// 当前发布版本的内置安全审查汇总。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityReviewReport {
    /// 报告对应的发布版本。
    pub version: String,
    /// 安全审查总体状态。
    pub status: String,
    /// 各边界的独立安全发现。
    pub findings: Vec<SecurityFinding>,
    /// 报告生成和覆盖范围审计记录。
    pub audit: Vec<String>,
}

impl SecurityFinding {
    /// 构造具有充分证据且已通过门禁的安全发现。
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

    /// 构造当前为警告并附带补救计划的跟踪发现。
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
    /// 统计高或严重级别且处于阻塞状态的发现数量。
    pub fn blocking_findings(&self) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.severity.is_required_gate() && finding.status.is_blocking())
            .count()
    }
}

#[cfg(test)]
/// 安全审查边界覆盖和强制门禁测试。
mod tests {
    use crate::ReleaseHardeningService;

    #[test]
    /// 验证关键安全边界均有发现且没有强制阻塞项。
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
