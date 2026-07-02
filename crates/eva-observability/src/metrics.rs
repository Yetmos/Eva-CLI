//! Metrics labels and sink traits.

use eva_core::EvaError;
use std::collections::BTreeMap;
use std::fmt;

/// Stable metric name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MetricName(String);

/// Metric label set with deterministic ordering.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MetricLabels {
    labels: BTreeMap<String, String>,
}

/// Metric semantic kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetricKind {
    Counter,
    Gauge,
    Histogram,
}

/// Single metric point handed to a sink.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricPoint {
    pub name: MetricName,
    pub kind: MetricKind,
    pub value: f64,
    pub labels: MetricLabels,
}

/// Sink trait implemented by future metrics backends.
pub trait MetricSink {
    fn record(&mut self, point: MetricPoint) -> Result<(), EvaError>;
}

impl MetricName {
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

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MetricName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl MetricLabels {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(key.into(), value.into());
        self
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.labels.get(key).map(String::as_str)
    }

    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.labels
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }
}

impl MetricKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Gauge => "gauge",
            Self::Histogram => "histogram",
        }
    }
}

impl MetricPoint {
    pub fn new(name: MetricName, kind: MetricKind, value: f64) -> Self {
        Self {
            name,
            kind,
            value,
            labels: MetricLabels::default(),
        }
    }

    pub fn with_labels(mut self, labels: MetricLabels) -> Self {
        self.labels = labels;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_name_rejects_unstable_values() {
        assert!(MetricName::parse("runtime.event.count").is_ok());
        assert!(MetricName::parse("runtime/event/count").is_err());
    }

    #[test]
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
}
