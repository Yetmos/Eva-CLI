//! 中文：指标名称、标签、数据点和可替换写入端契约。
//! Metrics labels and sink traits.

use eva_core::EvaError;
use std::collections::BTreeMap;
use std::fmt;

/// 中文：只允许稳定 ASCII 字符的指标名称。
/// Stable metric name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MetricName(
    /// 中文：经过校验、可安全用于文本协议和索引的原始名称。
    String,
);

/// 中文：使用有序映射保存、迭代顺序确定的指标标签集合。
/// Metric label set with deterministic ordering.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MetricLabels {
    /// 中文：标签键到值的有序映射，重复键采用最后写入值。
    labels: BTreeMap<String, String>,
}

/// 中文：指标点的语义类型。
/// Metric semantic kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetricKind {
    /// 中文：只应单调增加的累计计数。
    Counter,
    /// 中文：可增可减的瞬时测量值。
    Gauge,
    /// 中文：用于聚合分布的观测样本。
    Histogram,
}

/// 中文：交给指标写入端的一次完整观测点。
/// Single metric point handed to a sink.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricPoint {
    /// 中文：经过稳定字符校验的指标名称。
    pub name: MetricName,
    /// 中文：该数值的计数、仪表或直方图语义。
    pub kind: MetricKind,
    /// 中文：本次观测的浮点值。
    pub value: f64,
    /// 中文：用于切分指标序列的有序标签。
    pub labels: MetricLabels,
}

/// 中文：由具体指标后端实现的数据点写入接口。
/// Sink trait implemented by future metrics backends.
pub trait MetricSink {
    /// 中文：记录一个完整指标点，后端失败必须返回结构化错误。
    fn record(&mut self, point: MetricPoint) -> Result<(), EvaError>;
}

/// 中文：测试和冒烟流程使用的内存指标写入端。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct InMemoryMetricSink {
    /// 中文：按写入顺序保留的全部指标点。
    pub points: Vec<MetricPoint>,
}

impl MetricName {
    /// 中文：校验指标名非空、无边缘空白且只含跨后端稳定的 ASCII 字符。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        if value.is_empty() || value.trim() != value {
            return Err(EvaError::invalid_argument(
                "metric name cannot be empty or contain leading/trailing whitespace",
            ));
        }
        if !value
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '.' | '_' | '-'))
        {
            return Err(EvaError::invalid_argument(
                "metric name may only contain ASCII letters, digits, '.', '_', and '-'",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    /// 中文：返回经过校验的指标名称。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MetricName {
    /// 中文：把稳定指标名称原样写入格式化器。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl MetricLabels {
    /// 中文：创建空标签集合。
    pub fn new() -> Self {
        Self::default()
    }

    /// 中文：插入或覆盖一个标签，并返回集合以支持链式构造。
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(key.into(), value.into());
        self
    }

    /// 中文：按键读取标签值。
    pub fn get(&self, key: &str) -> Option<&str> {
        self.labels.get(key).map(String::as_str)
    }

    /// 中文：按键的字典序迭代标签，保证输出和测试结果确定。
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.labels
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    /// 中文：按确定顺序保留前 `max_labels` 个标签，并返回被丢弃数量。
    ///
    /// 此限制用于控制高基数后端的序列爆炸；因为底层是有序映射，相同输入总会保留
    /// 相同标签，而不会受插入顺序或 Hash 随机种子影响。
    pub fn limited(&self, max_labels: usize) -> (Self, usize) {
        let mut labels = BTreeMap::new();
        let mut dropped = 0;

        for (index, (key, value)) in self.labels.iter().enumerate() {
            if index < max_labels {
                labels.insert(key.clone(), value.clone());
            } else {
                dropped += 1;
            }
        }

        (Self { labels }, dropped)
    }

    /// 中文：构造运行时指标的标准低基数标签集合。
    pub fn runtime(runtime_mode: impl Into<String>, generation: impl Into<String>) -> Self {
        Self::new()
            .with("surface", "runtime")
            .with("runtime_mode", runtime_mode)
            .with("generation", generation)
    }

    /// 中文：构造 Provider 调用指标的标准标签集合。
    pub fn provider(
        adapter_id: impl Into<String>,
        capability: impl Into<String>,
        provider: impl Into<String>,
    ) -> Self {
        Self::new()
            .with("surface", "provider")
            .with("adapter_id", adapter_id)
            .with("capability", capability)
            .with("provider", provider)
    }

    /// 中文：构造任务状态指标的标准标签集合。
    pub fn task(status: impl Into<String>, agent_id: impl Into<String>) -> Self {
        Self::new()
            .with("surface", "task")
            .with("status", status)
            .with("agent_id", agent_id)
    }
}

impl MetricKind {
    /// 中文：返回后端和协议使用的稳定指标类型名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Gauge => "gauge",
            Self::Histogram => "histogram",
        }
    }
}

impl MetricPoint {
    /// 中文：创建没有标签的指标点。
    pub fn new(name: MetricName, kind: MetricKind, value: f64) -> Self {
        Self {
            name,
            kind,
            value,
            labels: MetricLabels::default(),
        }
    }

    /// 中文：替换指标点的完整标签集合。
    pub fn with_labels(mut self, labels: MetricLabels) -> Self {
        self.labels = labels;
        self
    }
}

impl MetricSink for InMemoryMetricSink {
    /// 中文：把数据点追加到内存列表；该实现不会产生外部失败。
    fn record(&mut self, point: MetricPoint) -> Result<(), EvaError> {
        self.points.push(point);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// 中文：验证稳定指标名接受点分名称并拒绝路径分隔符。
    fn metric_name_rejects_unstable_values() {
        assert!(MetricName::parse("runtime.event.count").is_ok());
        assert!(MetricName::parse("runtime/event/count").is_err());
    }

    #[test]
    /// 中文：验证标签输出按键排序而非插入顺序。
    fn labels_are_deterministically_ordered() {
        let labels = MetricLabels::new()
            .with("topic", "/input/user")
            .with("agent_id", "root-agent");

        let entries = labels.entries().collect::<Vec<_>>();

        assert_eq!(
            entries,
            vec![("agent_id", "root-agent"), ("topic", "/input/user")]
        );
    }

    #[test]
    /// 中文：验证指标点保留类型和值对应标签。
    fn metric_point_keeps_kind_and_labels() {
        let point = MetricPoint::new(
            MetricName::parse("runtime.event.accepted").unwrap(),
            MetricKind::Counter,
            1.0,
        )
        .with_labels(MetricLabels::new().with("topic", "/input/user"));

        assert_eq!(point.kind.as_str(), "counter");
        assert_eq!(point.labels.get("topic"), Some("/input/user"));
    }

    #[test]
    /// 中文：验证三个标准指标表面生成各自必需标签。
    fn metric_labels_cover_runtime_provider_and_task_surfaces() {
        let runtime = MetricLabels::runtime("basic", "gen-active");
        let provider = MetricLabels::provider("codex-cli", "code.review", "codex-cli");
        let task = MetricLabels::task("completed", "root-agent");

        assert_eq!(runtime.get("surface"), Some("runtime"));
        assert_eq!(provider.get("surface"), Some("provider"));
        assert_eq!(provider.get("capability"), Some("code.review"));
        assert_eq!(task.get("surface"), Some("task"));
    }

    #[test]
    /// 中文：验证基数限制按字典序稳定保留标签并报告丢弃数量。
    fn metric_labels_apply_deterministic_cardinality_limit() {
        let labels = MetricLabels::new()
            .with("zeta", "last")
            .with("alpha", "first")
            .with("surface", "runtime");

        let (limited, dropped) = labels.limited(2);

        assert_eq!(dropped, 1);
        assert_eq!(
            limited.entries().collect::<Vec<_>>(),
            vec![("alpha", "first"), ("surface", "runtime")]
        );
    }

    #[test]
    /// 中文：验证内存写入端按顺序保存指标点。
    fn in_memory_metric_sink_records_points() {
        let mut sink = InMemoryMetricSink::default();
        sink.record(MetricPoint::new(
            MetricName::parse("runtime.event.accepted").unwrap(),
            MetricKind::Counter,
            1.0,
        ))
        .unwrap();

        assert_eq!(sink.points.len(), 1);
    }
}
