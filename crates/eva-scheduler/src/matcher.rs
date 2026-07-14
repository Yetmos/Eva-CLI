//! 中文：主题与调度规则的匹配辅助函数。
//! Topic matching helpers.

use crate::routing::RoutingRule;
use eva_core::Topic;

/// 中文：本模块负责精确匹配和通配符主题匹配，不改变规则优先顺序。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "exact and wildcard Topic matching";

/// 中文：筛选模式匹配给定主题的规则，并保留配置中的原始规则顺序。
/// Returns route rules whose pattern matches `topic`, preserving route order.
pub fn matching_rules<'a>(rules: &'a [RoutingRule], topic: &Topic) -> Vec<&'a RoutingRule> {
    rules
        .iter()
        .filter(|rule| rule.pattern.matches(topic))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::{DeliveryMode, RoutingRule};
    use eva_core::{AgentId, TopicPattern};

    #[test]
    /// 中文：验证多层通配符规则能够匹配其命名空间下的具体主题。
    fn wildcard_rule_matches_topic() {
        let rule = RoutingRule::new(
            TopicPattern::parse("/sys/**").unwrap(),
            DeliveryMode::Fanout,
            vec![AgentId::parse("root-agent").unwrap()],
        );

        let rules = [rule];
        let matches = matching_rules(&rules, &Topic::parse("/sys/route-a").unwrap());

        assert_eq!(matches.len(), 1);
    }
}
