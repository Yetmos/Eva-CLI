//! 发布就绪度使用的性能预算基线。
//! Performance budget baselines for release readiness.

use crate::checklist::ReleaseGateStatus;

/// 由 consumer 维护的 release benchmark 预算策略项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseBenchmarkBudgetPolicy {
    /// workflow 与 evidence 共用的稳定组件键。
    pub component: &'static str,
    /// 必须与被测命令边界一致的指标名。
    pub metric: &'static str,
    /// consumer 唯一认可的最大耗时。
    pub budget_ms: u64,
}

/// 与 release workflow 一致、不能由 evidence producer 放宽的预算目录。
pub const RELEASE_BENCHMARK_BUDGET_POLICIES: [ReleaseBenchmarkBudgetPolicy; 2] = [
    ReleaseBenchmarkBudgetPolicy {
        component: "release.version",
        metric: "release binary version wall time",
        budget_ms: 2_000,
    },
    ReleaseBenchmarkBudgetPolicy {
        component: "release.check",
        metric: "release check wall time",
        budget_ms: 5_000,
    },
];

/// 按稳定组件与指标返回 consumer-owned benchmark 预算。
pub fn release_benchmark_budget_ms(component: &str, metric: &str) -> Option<u64> {
    RELEASE_BENCHMARK_BUDGET_POLICIES
        .iter()
        .find(|policy| policy.component == component && policy.metric == metric)
        .map(|policy| policy.budget_ms)
}

/// 性能观测是 synthetic 估计还是真实 measurement。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerformanceObservationKind {
    /// 静态估计或测试替身数值，不能证明运行时性能。
    Synthetic,
    /// 被评估为实际观测的数值；production 信任仍由 evidence verifier 授予。
    Measurement,
}

impl PerformanceObservationKind {
    /// 返回 CLI 与审计使用的稳定小写分类。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Synthetic => "synthetic",
            Self::Measurement => "measurement",
        }
    }
}

/// 与预算声明分离的一次性能观测。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerformanceObservation {
    /// 观测来源分类。
    pub kind: PerformanceObservationKind,
    /// 本次观测到的毫秒数。
    pub observed_ms: u64,
    /// 估计依据或真实测量命令、样本与环境。
    pub evidence: String,
}

/// 单个组件指标的预算、可选观测和发布门禁结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerformanceBudget {
    /// 被测组件或操作名称。
    pub component: String,
    /// 延迟等被测指标名称。
    pub metric: String,
    /// 允许的最大毫秒数。
    pub budget_ms: u64,
    /// 与预算声明分离的可选观测；None 表示完全未观测。
    pub observation: Option<PerformanceObservation>,
    /// measurement 按预算计算 Pass/Blocked；synthetic 或缺失观测为 Warn。
    pub status: ReleaseGateStatus,
    /// 预算或观测来源说明。
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
    /// 创建没有任何观测值的预算声明。
    pub fn unmeasured(
        component: impl Into<String>,
        metric: impl Into<String>,
        budget_ms: u64,
        evidence: impl Into<String>,
    ) -> Self {
        Self {
            component: component.into(),
            metric: metric.into(),
            budget_ms,
            observation: None,
            status: ReleaseGateStatus::Warn,
            evidence: evidence.into(),
        }
    }

    /// 创建只能用于 alpha 展示、不能声称实测通过的 synthetic 估计。
    pub fn synthetic(
        component: impl Into<String>,
        metric: impl Into<String>,
        budget_ms: u64,
        observed_ms: u64,
        evidence: impl Into<String>,
    ) -> Self {
        Self::with_observation(
            component,
            metric,
            budget_ms,
            PerformanceObservationKind::Synthetic,
            observed_ms,
            evidence,
        )
    }

    /// 创建标记为 measurement 并按阈值判定的预算结果。
    pub fn measured(
        component: impl Into<String>,
        metric: impl Into<String>,
        budget_ms: u64,
        observed_ms: u64,
        evidence: impl Into<String>,
    ) -> Self {
        Self::with_observation(
            component,
            metric,
            budget_ms,
            PerformanceObservationKind::Measurement,
            observed_ms,
            evidence,
        )
    }

    /// 返回实际或 synthetic 观测值；完全未观测时返回 None。
    pub fn observed_ms(&self) -> Option<u64> {
        self.observation
            .as_ref()
            .map(|observation| observation.observed_ms)
    }

    /// 返回 measurement、synthetic 或 unmeasured 稳定分类。
    pub fn observation_kind(&self) -> &'static str {
        self.observation
            .as_ref()
            .map(|observation| observation.kind.as_str())
            .unwrap_or("unmeasured")
    }

    /// 判断预算是否具有真实 measurement。
    pub fn is_measured(&self) -> bool {
        matches!(
            self.observation
                .as_ref()
                .map(|observation| observation.kind),
            Some(PerformanceObservationKind::Measurement)
        )
    }

    /// 判断真实 measurement 是否通过预算门禁。
    pub fn within_budget(&self) -> bool {
        self.is_measured() && self.status == ReleaseGateStatus::Pass
    }

    /// 判断真实 measurement 是否超过预算。
    pub fn is_over_budget(&self) -> bool {
        self.is_measured()
            && self
                .observed_ms()
                .map(|observed_ms| observed_ms > self.budget_ms)
                .unwrap_or(false)
    }

    /// 创建带明确分类的预算观测，并集中计算门禁状态。
    fn with_observation(
        component: impl Into<String>,
        metric: impl Into<String>,
        budget_ms: u64,
        kind: PerformanceObservationKind,
        observed_ms: u64,
        evidence: impl Into<String>,
    ) -> Self {
        let evidence = evidence.into();
        let status = match kind {
            PerformanceObservationKind::Synthetic => ReleaseGateStatus::Warn,
            PerformanceObservationKind::Measurement if observed_ms <= budget_ms => {
                ReleaseGateStatus::Pass
            }
            PerformanceObservationKind::Measurement => ReleaseGateStatus::Blocked,
        };
        Self {
            component: component.into(),
            metric: metric.into(),
            budget_ms,
            observation: Some(PerformanceObservation {
                kind,
                observed_ms,
                evidence: evidence.clone(),
            }),
            status,
            evidence,
        }
    }
}

impl PerformanceBaselineReport {
    /// 统计未通过预算的指标数量。
    pub fn over_budget_count(&self) -> usize {
        self.budgets
            .iter()
            .filter(|budget| budget.is_over_budget())
            .count()
    }

    /// 统计没有真实 measurement 的预算数量，包括 synthetic 估计。
    pub fn unmeasured_count(&self) -> usize {
        self.budgets
            .iter()
            .filter(|budget| !budget.is_measured())
            .count()
    }

    /// 统计具有真实 measurement 的预算数量。
    pub fn measured_count(&self) -> usize {
        self.budgets
            .iter()
            .filter(|budget| budget.is_measured())
            .count()
    }
}

#[cfg(test)]
/// 内置性能基线的预算回归测试。
mod tests {
    use crate::ReleaseHardeningService;

    #[test]
    /// 验证静态基线只声明预算，不伪造 observed_ms 或 measurement Pass。
    fn baseline_without_measurements_cannot_pass() {
        let report = ReleaseHardeningService::v15().performance_baseline();

        assert_eq!(report.status, "unmeasured");
        assert_eq!(report.over_budget_count(), 0);
        assert_eq!(report.measured_count(), 0);
        assert_eq!(report.unmeasured_count(), report.budgets.len());
        assert!(report.budgets.iter().all(|budget| {
            budget.status == crate::ReleaseGateStatus::Warn
                && budget.observation_kind() == "unmeasured"
                && budget.observed_ms().is_none()
                && !budget.within_budget()
        }));
        assert!(report
            .budgets
            .iter()
            .any(|budget| budget.component == "eventbus.publish"));
    }

    #[test]
    /// 验证只有真实 measurement 才能形成达标 Pass 或超预算 Blocked。
    fn real_measurements_distinguish_within_and_over_budget() {
        let within =
            crate::PerformanceBudget::measured("release.check", "latency", 200, 120, "run-1");
        let over =
            crate::PerformanceBudget::measured("release.check", "latency", 200, 250, "run-2");
        let missing =
            crate::PerformanceBudget::unmeasured("release.check", "latency", 200, "budget-only");
        let synthetic =
            crate::PerformanceBudget::synthetic("release.check", "latency", 200, 120, "estimate");

        assert!(within.within_budget());
        assert_eq!(within.status, crate::ReleaseGateStatus::Pass);
        assert!(over.is_over_budget());
        assert_eq!(over.status, crate::ReleaseGateStatus::Blocked);
        assert!(!missing.is_measured());
        assert_eq!(missing.observation_kind(), "unmeasured");
        assert_eq!(missing.status, crate::ReleaseGateStatus::Warn);
        assert!(!synthetic.is_measured());
        assert_eq!(synthetic.observation_kind(), "synthetic");
        assert_eq!(synthetic.status, crate::ReleaseGateStatus::Warn);
    }

    #[test]
    /// 验证 release workflow 的预算由 consumer 目录固定。
    fn release_benchmark_budget_policy_matches_workflow_contract() {
        assert_eq!(
            crate::release_benchmark_budget_ms(
                "release.version",
                "release binary version wall time"
            ),
            Some(2_000)
        );
        assert_eq!(
            crate::release_benchmark_budget_ms("release.check", "release check wall time"),
            Some(5_000)
        );
        assert_eq!(
            crate::release_benchmark_budget_ms("release.check", "forged metric"),
            None
        );
        let workflow = include_str!("../../../.github/workflows/release.yml");
        assert!(workflow.contains(
            "Measure-EvaCommand \"release.version\" \"release binary version wall time\" 2000"
        ));
        assert!(workflow
            .contains("Measure-EvaCommand \"release.check\" \"release check wall time\" 5000"));
    }
}
