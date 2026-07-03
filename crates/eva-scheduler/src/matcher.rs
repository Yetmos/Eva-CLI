//! Topic matching helpers.

use crate::routing::RoutingRule;
use eva_core::Topic;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "exact and wildcard Topic matching";

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
