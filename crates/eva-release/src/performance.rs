//! Performance budget baselines for release readiness.

use crate::checklist::ReleaseGateStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerformanceBudget {
    pub component: String,
    pub metric: String,
    pub budget_ms: u64,
    pub observed_ms: u64,
    pub status: ReleaseGateStatus,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerformanceBaselineReport {
    pub version: String,
    pub status: String,
    pub budgets: Vec<PerformanceBudget>,
    pub audit: Vec<String>,
}

impl PerformanceBudget {
    pub fn new(
        component: impl Into<String>,
        metric: impl Into<String>,
        budget_ms: u64,
        observed_ms: u64,
        evidence: impl Into<String>,
    ) -> Self {
        let status = if observed_ms <= budget_ms {
            ReleaseGateStatus::Pass
        } else {
            ReleaseGateStatus::Blocked
        };
        Self {
            component: component.into(),
            metric: metric.into(),
            budget_ms,
            observed_ms,
            status,
            evidence: evidence.into(),
        }
    }

    pub fn within_budget(&self) -> bool {
        self.status == ReleaseGateStatus::Pass
    }
}

impl PerformanceBaselineReport {
    pub fn over_budget_count(&self) -> usize {
        self.budgets
            .iter()
            .filter(|budget| !budget.within_budget())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use crate::ReleaseHardeningService;

    #[test]
    fn baseline_respects_all_budgets() {
        let report = ReleaseHardeningService::v15().performance_baseline();

        assert_eq!(report.status, "within_budget");
        assert_eq!(report.over_budget_count(), 0);
        assert!(report
            .budgets
            .iter()
            .any(|budget| budget.component == "eventbus.publish"));
    }
}
