//! 中文：Topic 名称、pattern 与通配符匹配契约。
//! English: Topic names, patterns, and wildcard matching.

use crate::error::EvaError;
use std::fmt;
use std::str::FromStr;

/// 中文：具体事件 topic，只表示已发生或待投递的实际路径。
/// English: A concrete event topic representing an actual emitted or deliverable path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Topic {
    // 中文：保留原始绝对路径，保证 Display、日志和配置回写稳定。
    // English: Preserve the original absolute path for stable Display, logs, and config round-tripping.
    path: String,
    // 中文：缓存已校验分段，供匹配逻辑避免重复 split。
    // English: Cache validated segments so matching logic does not repeatedly split.
    segments: Vec<String>,
}

impl Topic {
    /// 中文：解析并校验具体 topic。
    /// English: Parses and validates a concrete topic.
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        let segments = parse_segments(value, SegmentMode::Topic)?;
        Ok(Self {
            path: value.to_owned(),
            segments,
        })
    }

    /// 中文：从 owned 或 borrowed 字符串创建 topic。
    /// English: Creates a topic from an owned or borrowed string.
    pub fn new(value: impl Into<String>) -> Result<Self, EvaError> {
        let value = value.into();
        Self::parse(&value)
    }

    /// 中文：返回已校验的 topic 路径。
    /// English: Returns the validated topic path.
    pub fn as_str(&self) -> &str {
        &self.path
    }

    /// 中文：返回已校验的 topic 分段。
    /// English: Returns the validated topic segments.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.segments.iter().map(String::as_str)
    }

    fn segment_count(&self) -> usize {
        self.segments.len()
    }
}

impl fmt::Display for Topic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.path)
    }
}

impl FromStr for Topic {
    type Err = EvaError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for Topic {
    type Error = EvaError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

/// 中文：topic pattern 中已校验的单个分段。
/// English: A validated segment in a topic pattern.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TopicPatternSegment {
    /// 中文：精确匹配一个分段。
    /// English: An exact segment match.
    Exact(String),
    /// 中文：`*`，精确匹配一个分段。
    /// English: `*`, matching exactly one segment.
    SingleWildcard,
    /// 中文：`**`，匹配零个或多个尾部分段。
    /// English: `**`, matching zero or more trailing segments.
    TailWildcard,
}

impl TopicPatternSegment {
    /// 中文：返回该分段在 pattern 中的文本形式。
    /// English: Returns the segment as it appears in a pattern.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Exact(segment) => segment.as_str(),
            Self::SingleWildcard => "*",
            Self::TailWildcard => "**",
        }
    }

    fn is_wildcard(&self) -> bool {
        matches!(self, Self::SingleWildcard | Self::TailWildcard)
    }
}

/// 中文：用于订阅或路由的 topic pattern。
/// English: A topic subscription or routing pattern.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TopicPattern {
    // 中文：保留原始 pattern，方便展示与精确 pattern 转换。
    // English: Preserve the original pattern for display and exact-pattern conversion.
    pattern: String,
    // 中文：预解析 pattern 分段，保证匹配时只做线性扫描。
    // English: Pre-parse pattern segments so matching is a linear scan.
    segments: Vec<TopicPatternSegment>,
}

impl TopicPattern {
    /// 中文：解析并校验 topic pattern。
    /// English: Parses and validates a topic pattern.
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        let raw_segments = parse_segments(value, SegmentMode::Pattern)?;
        let last_index = raw_segments.len().saturating_sub(1);
        let mut segments = Vec::with_capacity(raw_segments.len());

        for (index, segment) in raw_segments.into_iter().enumerate() {
            // 中文：`**` 只允许作为最后一段，避免中间贪婪匹配造成歧义。
            // English: `**` is allowed only at the tail to avoid ambiguous greedy matching in the middle.
            let parsed = match segment.as_str() {
                "*" => TopicPatternSegment::SingleWildcard,
                "**" if index == last_index => TopicPatternSegment::TailWildcard,
                "**" => {
                    return Err(topic_error(
                        value,
                        "tail wildcard '**' is only allowed as the final segment",
                    ))
                }
                exact => TopicPatternSegment::Exact(exact.to_owned()),
            };
            segments.push(parsed);
        }

        Ok(Self {
            pattern: value.to_owned(),
            segments,
        })
    }

    /// 中文：从 owned 或 borrowed 字符串创建 pattern。
    /// English: Creates a pattern from an owned or borrowed string.
    pub fn new(value: impl Into<String>) -> Result<Self, EvaError> {
        let value = value.into();
        Self::parse(&value)
    }

    /// 中文：返回已校验的 pattern。
    /// English: Returns the validated pattern.
    pub fn as_str(&self) -> &str {
        &self.pattern
    }

    /// 中文：返回已校验的 pattern 分段。
    /// English: Returns the validated pattern segments.
    pub fn segments(&self) -> &[TopicPatternSegment] {
        &self.segments
    }

    /// 中文：pattern 不包含通配符时返回 true。
    /// English: Returns true when this pattern contains no wildcards.
    pub fn is_exact(&self) -> bool {
        self.segments.iter().all(|segment| !segment.is_wildcard())
    }

    /// 中文：将精确 pattern 转换成具体 topic；含通配符时返回 None。
    /// English: Converts an exact pattern to a concrete topic; returns None when wildcards are present.
    pub fn as_exact_topic(&self) -> Option<Topic> {
        if self.is_exact() {
            Topic::parse(&self.pattern).ok()
        } else {
            None
        }
    }

    /// 中文：pattern 匹配具体 topic 时返回 true。
    /// English: Returns true when the pattern matches a concrete topic.
    pub fn matches(&self, topic: &Topic) -> bool {
        let mut topic_index = 0usize;

        for pattern_segment in &self.segments {
            match pattern_segment {
                TopicPatternSegment::Exact(expected) => {
                    // 中文：精确分段必须与当前位置 topic 分段完全相等。
                    // English: Exact segments must equal the topic segment at the current position.
                    if topic.segments.get(topic_index) != Some(expected) {
                        return false;
                    }
                    topic_index += 1;
                }
                TopicPatternSegment::SingleWildcard => {
                    // 中文：`*` 必须消耗一个且仅一个 topic 分段。
                    // English: `*` must consume exactly one topic segment.
                    if topic_index >= topic.segment_count() {
                        return false;
                    }
                    topic_index += 1;
                }
                // 中文：尾部 `**` 吸收剩余所有分段，也允许剩余为零。
                // English: Tail `**` absorbs all remaining segments, including zero segments.
                TopicPatternSegment::TailWildcard => return true,
            }
        }

        // 中文：没有尾部 `**` 时，pattern 和 topic 必须同时消耗完。
        // English: Without tail `**`, both pattern and topic must be fully consumed.
        topic_index == topic.segment_count()
    }
}

impl fmt::Display for TopicPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.pattern)
    }
}

impl FromStr for TopicPattern {
    type Err = EvaError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for TopicPattern {
    type Error = EvaError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentMode {
    // 中文：具体 topic 模式禁止任何通配符。
    // English: Concrete topic mode forbids all wildcards.
    Topic,
    // 中文：pattern 模式允许整段 `*` 和尾部 `**`。
    // English: Pattern mode allows whole-segment `*` and tail `**`.
    Pattern,
}

fn parse_segments(value: &str, mode: SegmentMode) -> Result<Vec<String>, EvaError> {
    // 中文：Topic/path 必须是绝对路径形式，避免相对路径在不同调用点含义不同。
    // English: Topic paths must be absolute so relative paths cannot mean different things at different call sites.
    if value.is_empty() {
        return Err(topic_error(value, "topic path cannot be empty"));
    }

    if !value.starts_with('/') {
        return Err(topic_error(value, "topic path must start with '/'"));
    }

    if value.len() == 1 {
        return Err(topic_error(
            value,
            "topic path must contain at least one segment",
        ));
    }

    if value.ends_with('/') {
        return Err(topic_error(value, "topic path must not end with '/'"));
    }

    let mut segments = Vec::new();
    for segment in value[1..].split('/') {
        // 中文：空分段会让 `/a//b` 与 `/a/b` 的含义不稳定，因此拒绝。
        // English: Empty segments make `/a//b` and `/a/b` semantically unstable, so reject them.
        if segment.is_empty() {
            return Err(topic_error(
                value,
                "topic path must not contain empty segments",
            ));
        }

        match mode {
            SegmentMode::Topic => validate_topic_segment(value, segment)?,
            SegmentMode::Pattern => validate_pattern_segment(value, segment)?,
        }

        segments.push(segment.to_owned());
    }

    Ok(segments)
}

fn validate_topic_segment(topic: &str, segment: &str) -> Result<(), EvaError> {
    // 中文：具体 topic 不能含通配符；通配符只属于订阅/路由 pattern。
    // English: Concrete topics cannot contain wildcards; wildcards belong only to subscription/routing patterns.
    if segment == "*" || segment == "**" || segment.contains('*') {
        return Err(topic_error(
            topic,
            "concrete topic segments cannot contain wildcards",
        ));
    }

    validate_plain_segment(topic, segment)
}

fn validate_pattern_segment(pattern: &str, segment: &str) -> Result<(), EvaError> {
    // 中文：pattern 通配符必须占据完整分段，避免 `foo*` 这类部分匹配规则膨胀。
    // English: Pattern wildcards must occupy a whole segment to avoid expanding into partial-match rules such as `foo*`.
    if segment == "*" || segment == "**" {
        return Ok(());
    }

    if segment.contains('*') {
        return Err(topic_error(
            pattern,
            "wildcards must occupy an entire pattern segment",
        ));
    }

    validate_plain_segment(pattern, segment)
}

fn validate_plain_segment(topic: &str, segment: &str) -> Result<(), EvaError> {
    // 中文：分段内不允许空白，保持配置、CLI 参数和日志中的 topic 可直接复制使用。
    // English: Segment whitespace is forbidden so topics can be copied directly across config, CLI arguments, and logs.
    if segment.chars().any(char::is_whitespace) {
        return Err(topic_error(
            topic,
            "topic segments cannot contain whitespace",
        ));
    }

    Ok(())
}

fn topic_error(topic: &str, message: &str) -> EvaError {
    EvaError::invalid_argument(message).with_context("topic", topic)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_topic_accepts_absolute_segments() {
        let topic = Topic::parse("/input/user").unwrap();
        assert_eq!(topic.as_str(), "/input/user");
        assert_eq!(topic.to_string(), "/input/user");
        assert_eq!(topic.segments().collect::<Vec<_>>(), ["input", "user"]);
    }

    #[test]
    fn parse_topic_rejects_relative_path() {
        let error = Topic::parse("input/user").unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::InvalidArgument);
    }

    #[test]
    fn parse_topic_rejects_empty_segment() {
        let error = Topic::parse("/input//user").unwrap_err();
        assert!(error.message().contains("empty segments"));
    }

    #[test]
    fn parse_topic_rejects_wildcard() {
        let error = Topic::parse("/agent/*").unwrap_err();
        assert!(error.message().contains("wildcards"));
    }

    #[test]
    fn parse_topic_rejects_trailing_slash() {
        let error = Topic::parse("/a/b/").unwrap_err();
        assert!(error.message().contains("end with"));
    }

    #[test]
    fn pattern_matches_exact_topic() {
        let pattern = TopicPattern::parse("/a/b").unwrap();
        assert!(pattern.matches(&Topic::parse("/a/b").unwrap()));
        assert!(!pattern.matches(&Topic::parse("/a/c").unwrap()));
        assert!(pattern.is_exact());
        assert_eq!(pattern.as_exact_topic().unwrap().as_str(), "/a/b");
    }

    #[test]
    fn pattern_matches_single_wildcard() {
        let pattern = TopicPattern::parse("/a/*").unwrap();
        assert!(pattern.matches(&Topic::parse("/a/b").unwrap()));
        assert!(!pattern.matches(&Topic::parse("/a/b/c").unwrap()));
        assert!(!pattern.matches(&Topic::parse("/a").unwrap()));
    }

    #[test]
    fn pattern_matches_tail_wildcard() {
        let pattern = TopicPattern::parse("/a/**").unwrap();
        assert!(pattern.matches(&Topic::parse("/a").unwrap()));
        assert!(pattern.matches(&Topic::parse("/a/b").unwrap()));
        assert!(pattern.matches(&Topic::parse("/a/b/c").unwrap()));
        assert!(!pattern.is_exact());
        assert!(pattern.as_exact_topic().is_none());
    }

    #[test]
    fn pattern_rejects_middle_tail_wildcard() {
        let error = TopicPattern::parse("/a/**/c").unwrap_err();
        assert!(error.message().contains("final segment"));
    }

    #[test]
    fn pattern_rejects_embedded_wildcard() {
        let error = TopicPattern::parse("/sys/ro*").unwrap_err();
        assert!(error.message().contains("entire pattern segment"));
    }

    #[test]
    fn pattern_display_is_stable() {
        let pattern: TopicPattern = "/agent/*/event".parse().unwrap();
        assert_eq!(pattern.to_string(), "/agent/*/event");
        assert!(matches!(
            pattern.segments()[1],
            TopicPatternSegment::SingleWildcard
        ));
    }
}
