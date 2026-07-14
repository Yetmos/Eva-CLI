//! 中文：主题路由表的 YAML 加载、校验和规范化。
//! Topic route table loading and normalization.

use crate::{read_yaml_file, with_field_context, EvaError};
use eva_core::{AgentId, TopicPattern};
use serde::Deserialize;
use serde_yaml::Value;
use std::path::{Path, PathBuf};

/// 中文：写入路由配置错误上下文的稳定类型名称。
const CONFIG_TYPE: &str = "Topic routes";

/// 中文：已校验、可直接注册到调度器的有序路由表。
/// Validated route table ready for scheduler registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteConfig {
    /// 中文：源路由文件路径，用于精确报告字段错误。
    /// Path to the source route file.
    pub path: PathBuf,
    /// 中文：调度器按此顺序评估的规范化路由规则。
    /// Ordered rules evaluated by the scheduler.
    pub routes: Vec<RouteRule>,
}

/// 中文：一条主题模式、投递方式和目标 Agent 组成的路由规则。
/// One topic route rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRule {
    /// 中文：经过语法校验的主题匹配模式。
    pub pattern: TopicPattern,
    /// 中文：匹配成功后的广播或竞争投递语义。
    pub delivery: RouteDelivery,
    /// 中文：按配置顺序排列的目标 Agent，保证至少有一个元素。
    pub agents: Vec<AgentId>,
}

/// 中文：路由匹配后支持的投递行为。
/// Supported delivery behavior for a matched route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RouteDelivery {
    /// 中文：向规则列出的全部 Agent 分发事件。
    Fanout,
    /// 中文：由规则列出的一个 Agent 竞争处理事件。
    Compete,
}

/// 中文：为 Schema 对齐测试保留的原始投递类型别名。
/// Raw route delivery spelling retained for schema alignment tests.
pub type RawRouteDelivery = RouteDelivery;

/// 中文：读取并完整校验主题路由表，任何规则无效都会使整份配置失败。
/// Loads and validates a topic route table.
pub fn load_routes(path: impl AsRef<Path>) -> Result<RouteConfig, EvaError> {
    let path = path.as_ref();
    let raw: RawRouteConfig = read_yaml_file(path, CONFIG_TYPE)?;
    RouteConfig::try_from_raw(path.to_path_buf(), raw)
}

impl RouteConfig {
    /// 中文：要求至少一条规则，并按原顺序逐条规范化以保留调度优先级。
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
    /// 中文：校验单条规则的主题、投递方式和非空 Agent 列表，并附加数组下标上下文。
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
    /// 中文：解析受支持的稳定投递名称，未知值作为不支持错误返回。
    /// Parses a supported route delivery mode.
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "fanout" => Ok(Self::Fanout),
            "compete" => Ok(Self::Compete),
            _ => Err(EvaError::unsupported("unsupported route delivery mode")
                .with_context("delivery", value)),
        }
    }

    /// 中文：返回写入 YAML 和协议输出的稳定拼写。
    /// Returns the stable YAML spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fanout => "fanout",
            Self::Compete => "compete",
        }
    }
}

/// 中文：反序列化阶段使用的路由表结构，随后还需执行语义校验。
#[derive(Debug, Deserialize)]
struct RawRouteConfig {
    /// 中文：保持 YAML 顺序的原始规则列表。
    routes: Vec<RawRouteRule>,
}

/// 中文：尚未解析主题、投递枚举和 Agent 标识的原始路由规则。
#[derive(Debug, Deserialize)]
struct RawRouteRule {
    /// 中文：原始主题模式文本。
    pattern: String,
    /// 中文：原始投递方式文本。
    delivery: String,
    /// 中文：原始 Agent 标识列表，缺失时按空列表处理并在语义校验中拒绝。
    #[serde(default)]
    agents: Vec<String>,
}

impl TryFrom<Value> for RouteConfig {
    /// 中文：内存 YAML 转换使用的结构化错误类型。
    type Error = EvaError;

    /// 中文：先按原始结构反序列化，再执行与文件加载路径相同的语义校验。
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

    /// 中文：返回路由配置测试使用的工作区根目录。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    /// 中文：验证仓库样例路由的顺序、模式和目标均正确加载。
    fn load_routes_accepts_sample_routes() {
        let routes = load_routes(workspace_root().join("config/routes/topics.yaml")).unwrap();

        assert_eq!(routes.routes.len(), 4);
        assert_eq!(routes.routes[0].pattern.as_str(), "/input/user");
        assert_eq!(routes.routes[0].delivery, RouteDelivery::Fanout);
        assert_eq!(routes.routes[0].agents[0].as_str(), "root-agent");
    }

    #[test]
    /// 中文：验证不符合主题路径语法的模式被拒绝。
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
    /// 中文：验证未知投递方式返回不支持错误。
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
    /// 中文：验证没有目标 Agent 的规则不会进入调度器。
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
