//! Topic route table loading and normalization.

use crate::{read_yaml_file, with_field_context, EvaError};
use eva_core::{AgentId, TopicPattern};
use serde::Deserialize;
use serde_yaml::Value;
use std::path::{Path, PathBuf};

const CONFIG_TYPE: &str = "Topic routes";

/// Validated route table ready for scheduler registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteConfig {
    /// Path to the source route file.
    pub path: PathBuf,
    /// Ordered rules evaluated by the scheduler.
    pub routes: Vec<RouteRule>,
}

/// One topic route rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRule {
    pub pattern: TopicPattern,
    pub delivery: RouteDelivery,
    pub agents: Vec<AgentId>,
}

/// Supported delivery behavior for a matched route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RouteDelivery {
    Fanout,
    Compete,
}

/// Raw route delivery spelling retained for schema alignment tests.
pub type RawRouteDelivery = RouteDelivery;

/// Loads and validates a topic route table.
pub fn load_routes(path: impl AsRef<Path>) -> Result<RouteConfig, EvaError> {
    let path = path.as_ref();
    let raw: RawRouteConfig = read_yaml_file(path, CONFIG_TYPE)?;
    RouteConfig::try_from_raw(path.to_path_buf(), raw)
}

impl RouteConfig {
    fn try_from_raw(path: PathBuf, raw: RawRouteConfig) -> Result<Self, EvaError> {
        if raw.routes.is_empty() {
            return Err(
                EvaError::invalid_argument("route table must contain at least one route")
                    .with_context("config_type", CONFIG_TYPE)
                    .with_context("path", path.display().to_string())
                    .with_context("field", "routes"),
            );
        }

        let routes = raw
            .routes
            .into_iter()
            .enumerate()
            .map(|(index, route)| RouteRule::try_from_raw(&path, index, route))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self { path, routes })
    }
}

impl RouteRule {
    fn try_from_raw(path: &Path, index: usize, raw: RawRouteRule) -> Result<Self, EvaError> {
        let field_prefix = format!("routes[{index}]");
        let pattern = TopicPattern::parse(&raw.pattern).map_err(|error| {
            with_field_context(error, CONFIG_TYPE, path, format!("{field_prefix}.pattern"))
        })?;
        let delivery = RouteDelivery::parse(&raw.delivery).map_err(|error| {
            with_field_context(error, CONFIG_TYPE, path, format!("{field_prefix}.delivery"))
        })?;
        if raw.agents.is_empty() {
            return Err(
                EvaError::invalid_argument("route must target at least one Agent")
                    .with_context("config_type", CONFIG_TYPE)
                    .with_context("path", path.display().to_string())
                    .with_context("field", format!("{field_prefix}.agents")),
            );
        }
        let agents = raw
            .agents
            .iter()
            .map(|agent| AgentId::parse(agent))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| {
                with_field_context(error, CONFIG_TYPE, path, format!("{field_prefix}.agents"))
            })?;

        Ok(Self {
            pattern,
            delivery,
            agents,
        })
    }
}

impl RouteDelivery {
    /// Parses a supported route delivery mode.
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "fanout" => Ok(Self::Fanout),
            "compete" => Ok(Self::Compete),
            _ => Err(EvaError::unsupported("unsupported route delivery mode")
                .with_context("delivery", value)),
        }
    }

    /// Returns the stable YAML spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fanout => "fanout",
            Self::Compete => "compete",
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawRouteConfig {
    routes: Vec<RawRouteRule>,
}

#[derive(Debug, Deserialize)]
struct RawRouteRule {
    pattern: String,
    delivery: String,
    #[serde(default)]
    agents: Vec<String>,
}

impl TryFrom<Value> for RouteConfig {
    type Error = EvaError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let raw = RawRouteConfig::deserialize(value).map_err(|error| {
            EvaError::invalid_argument("failed to parse Topic routes")
                .with_context("yaml_error", error.to_string())
        })?;
        Self::try_from_raw(PathBuf::new(), raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::ErrorKind;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    fn load_routes_accepts_sample_routes() {
        let routes = load_routes(workspace_root().join("config/routes/topics.yaml")).unwrap();

        assert_eq!(routes.routes.len(), 4);
        assert_eq!(routes.routes[0].pattern.as_str(), "/input/user");
        assert_eq!(routes.routes[0].delivery, RouteDelivery::Fanout);
        assert_eq!(routes.routes[0].agents[0].as_str(), "root-agent");
    }

    #[test]
    fn route_config_rejects_invalid_pattern() {
        let value = serde_yaml::from_str::<Value>(
            r#"
routes:
  - pattern: sys
    delivery: fanout
    agents:
      - root-agent
"#,
        )
        .unwrap();

        let error = RouteConfig::try_from(value).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

    #[test]
    fn route_config_rejects_unknown_delivery() {
        let value = serde_yaml::from_str::<Value>(
            r#"
routes:
  - pattern: /sys
    delivery: random
    agents:
      - root-agent
"#,
        )
        .unwrap();

        let error = RouteConfig::try_from(value).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn route_config_rejects_empty_agents() {
        let value = serde_yaml::from_str::<Value>(
            r#"
routes:
  - pattern: /sys
    delivery: fanout
    agents: []
"#,
        )
        .unwrap();

        let error = RouteConfig::try_from(value).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }
}
