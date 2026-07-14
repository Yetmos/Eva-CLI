//! 发布就绪度使用的性能预算基线。
//! Performance budget baselines for release readiness.

use crate::checklist::ReleaseGateStatus;

/// 单个组件指标的预算、观测值和发布门禁结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerformanceBudget {
    /// 被测组件或操作名称。
    pub component: String,
    /// 延迟等被测指标名称。
    pub metric: String,
    /// 允许的最大毫秒数。
    pub budget_ms: u64,
    /// 本次证据观测到的毫秒数。
    pub observed_ms: u64,
    /// 根据观测值与预算计算的门禁状态。
    pub status: ReleaseGateStatus,
    /// 测量来源或测试命令说明。
    pub evidence: String,
}

/// 一个版本的性能预算汇总报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerformanceBaselineReport {
    /// 报告对应的发布版本。
    pub version: String,
    /// 总体预算状态。
    pub status: String,
    /// 各组件性能预算。
    pub budgets: Vec<PerformanceBudget>,
    /// 报告生成和证据来源审计记录。
    pub audit: Vec<String>,
}

impl PerformanceBudget {
    /// 创建预算并根据 `observed_ms <= budget_ms` 计算门禁状态。
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

    /// 判断观测值是否通过预算门禁。
    pub fn within_budget(&self) -> bool {
        self.status == ReleaseGateStatus::Pass
    }
}

impl PerformanceBaselineReport {
    /// 统计未通过预算的指标数量。
    pub fn over_budget_count(&self) -> usize {
        self.budgets
            .iter()
            .filter(|budget| !budget.within_budget())
            .count()
    }
}

#[cfg(test)]
/// 内置性能基线的预算回归测试。
mod tests {
    use crate::ReleaseHardeningService;

    #[test]
    /// 验证当前发布基线的所有观测值均在预算内。
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
