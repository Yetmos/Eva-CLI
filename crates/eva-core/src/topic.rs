//! Topic names, patterns, and wildcard matching.

use crate::error::EvaError;
use std::fmt;
use std::str::FromStr;

/// A concrete event topic.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Topic {
    path: String,
    segments: Vec<String>,
}

impl Topic {
    /// Parses and validates a concrete topic.
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        let segments = parse_segments(value, SegmentMode::Topic)?;
        Ok(Self {
            path: value.to_owned(),
            segments,
        })
    }

    /// Creates a topic from an owned or borrowed string.
    pub fn new(value: impl Into<String>) -> Result<Self, EvaError> {
        let value = value.into();
        Self::parse(&value)
    }

    /// Returns the normalized topic path.
    pub fn as_str(&self) -> &str {
        &self.path
    }

    /// Returns the validated topic segments.
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

/// A validated segment in a topic pattern.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TopicPatternSegment {
    /// An exact segment match.
    Exact(String),
    /// `*`, matching exactly one segment.
    SingleWildcard,
    /// `**`, matching zero or more trailing segments.
    TailWildcard,
}

impl TopicPatternSegment {
    /// Returns the segment as it appears in a pattern.
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

/// A topic subscription or routing pattern.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TopicPattern {
    pattern: String,
    segments: Vec<TopicPatternSegment>,
}

impl TopicPattern {
    /// Parses and validates a topic pattern.
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        let raw_segments = parse_segments(value, SegmentMode::Pattern)?;
        let last_index = raw_segments.len().saturating_sub(1);
        let mut segments = Vec::with_capacity(raw_segments.len());

        for (index, segment) in raw_segments.into_iter().enumerate() {
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

    /// Creates a pattern from an owned or borrowed string.
    pub fn new(value: impl Into<String>) -> Result<Self, EvaError> {
        let value = value.into();
        Self::parse(&value)
    }

    /// Returns the normalized pattern.
    pub fn as_str(&self) -> &str {
        &self.pattern
    }

    /// Returns the validated pattern segments.
    pub fn segments(&self) -> &[TopicPatternSegment] {
        &self.segments
    }

    /// Returns true when this pattern contains no wildcards.
    pub fn is_exact(&self) -> bool {
        self.segments.iter().all(|segment| !segment.is_wildcard())
    }

    /// Converts an exact pattern to a concrete topic.
    pub fn as_exact_topic(&self) -> Option<Topic> {
        if self.is_exact() {
            Topic::parse(&self.pattern).ok()
        } else {
            None
        }
    }

    /// Returns true when the pattern matches a concrete topic.
    pub fn matches(&self, topic: &Topic) -> bool {
        let mut topic_index = 0usize;

        for pattern_segment in &self.segments {
            match pattern_segment {
                TopicPatternSegment::Exact(expected) => {
                    if topic.segments.get(topic_index) != Some(expected) {
                        return false;
                    }
                    topic_index += 1;
                }
                TopicPatternSegment::SingleWildcard => {
                    if topic_index >= topic.segment_count() {
                        return false;
                    }
                    topic_index += 1;
                }
                TopicPatternSegment::TailWildcard => return true,
            }
        }

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
    Topic,
    Pattern,
}

fn parse_segments(value: &str, mode: SegmentMode) -> Result<Vec<String>, EvaError> {
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
    if segment == "*" || segment == "**" || segment.contains('*') {
        return Err(topic_error(
            topic,
            "concrete topic segments cannot contain wildcards",
        ));
    }

    validate_plain_segment(topic, segment)
}

fn validate_pattern_segment(pattern: &str, segment: &str) -> Result<(), EvaError> {
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
