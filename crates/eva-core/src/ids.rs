//! 中文：Eva 模块间共享的强类型 identifier。
//! English: Strongly typed identifiers shared across Eva modules.

use crate::error::EvaError;
use std::fmt;
use std::str::FromStr;

const MAX_ID_LEN: usize = 128;

// 中文：所有 ID 类型共享同一套校验规则，但保持不同 Rust 类型，防止把 AgentId/AdapterId 混用。
// English: All ID types share validation rules while remaining distinct Rust types to prevent AgentId/AdapterId mixups.
macro_rules! define_id {
    ($name:ident, $label:literal) => {
        #[doc = concat!("中文：稳定的 ", $label, " identifier。")]
        #[doc = concat!("English: Stable ", $label, " identifier.")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            #[doc = concat!("中文：解析并校验 ", $label, " identifier。")]
            #[doc = concat!("English: Parses and validates a ", $label, " identifier.")]
            pub fn parse(value: &str) -> Result<Self, EvaError> {
                validate_id($label, value)?;
                Ok(Self(value.to_owned()))
            }

            #[doc = concat!("中文：从 owned 或 borrowed 字符串创建 ", $label, " identifier。")]
            #[doc = concat!("English: Creates a ", $label, " identifier from an owned or borrowed string.")]
            pub fn new(value: impl Into<String>) -> Result<Self, EvaError> {
                let value = value.into();
                Self::parse(&value)
            }

            /// 中文：以字符串切片返回 identifier。
            /// English: Returns the identifier as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = EvaError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = EvaError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::parse(value)
            }
        }
    };
}

define_id!(AgentId, "agent");
define_id!(AdapterId, "adapter");
define_id!(CapabilityId, "capability");
define_id!(RequestId, "request");
define_id!(EventId, "event");
define_id!(GenerationId, "generation");

fn validate_id(label: &str, value: &str) -> Result<(), EvaError> {
    // 中文：ID 会进入配置、日志、路径映射和外部协议字段，先排除空值与首尾空白。
    // English: IDs appear in config, logs, path mapping, and external protocol fields, so reject empty values and edge whitespace first.
    if value.is_empty() {
        return Err(id_error(label, value, "identifier cannot be empty"));
    }

    if value.trim() != value {
        return Err(id_error(
            label,
            value,
            "identifier cannot contain leading or trailing whitespace",
        ));
    }

    // 中文：长度上限防止日志/状态文件被异常 identifier 放大。
    // English: The length cap prevents abnormal identifiers from inflating logs or state files.
    if value.len() > MAX_ID_LEN {
        return Err(id_error(label, value, "identifier is too long"));
    }

    // 中文：禁止路径分隔符，避免 ID 被误用为相对路径片段。
    // English: Path separators are forbidden so IDs cannot be mistaken for relative path fragments.
    if value.contains('/') || value.contains('\\') {
        return Err(id_error(
            label,
            value,
            "identifier cannot contain path separators",
        ));
    }

    // 中文：只允许跨平台稳定字符；需要展示名时应使用单独字段，不要放进 ID。
    // English: Allow only cross-platform stable characters; human display names should live in separate fields.
    if !value.chars().all(is_stable_id_char) {
        return Err(id_error(
            label,
            value,
            "identifier may only contain ASCII letters, digits, '.', '_', '-', and ':'",
        ));
    }

    Ok(())
}

fn is_stable_id_char(value: char) -> bool {
    value.is_ascii_alphanumeric() || matches!(value, '.' | '_' | '-' | ':')
}

fn id_error(label: &str, value: &str, message: &str) -> EvaError {
    EvaError::invalid_argument(message)
        .with_context("id_type", label)
        .with_context("id", value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn agent_id_accepts_stable_slug() {
        let id = AgentId::parse("agent-root").unwrap();
        assert_eq!(id.as_str(), "agent-root");
        assert_eq!(id.to_string(), "agent-root");
    }

    #[test]
    fn agent_id_rejects_empty() {
        let error = AgentId::parse("").unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::InvalidArgument);
    }

    #[test]
    fn agent_id_rejects_whitespace() {
        assert!(AgentId::parse("agent root").is_err());
        assert!(AgentId::parse(" agent-root").is_err());
    }

    #[test]
    fn agent_id_rejects_path_separator() {
        assert!(AgentId::parse("agent/root").is_err());
        assert!(AgentId::parse("agent\\root").is_err());
    }

    #[test]
    fn request_id_display_is_stable() {
        let id: RequestId = "req-001".parse().unwrap();
        assert_eq!(id.to_string(), "req-001");
    }

    #[test]
    fn all_id_types_parse_stable_values() {
        assert_eq!(
            AdapterId::parse("adapter-cli").unwrap().as_str(),
            "adapter-cli"
        );
        assert_eq!(
            CapabilityId::parse("repo.summary").unwrap().as_str(),
            "repo.summary"
        );
        assert_eq!(EventId::parse("evt-1").unwrap().as_str(), "evt-1");
        assert_eq!(GenerationId::parse("gen-1").unwrap().as_str(), "gen-1");
    }

    #[test]
    fn ids_can_be_hash_keys() {
        let mut ids = HashSet::new();
        ids.insert(EventId::parse("evt-1").unwrap());
        ids.insert(EventId::parse("evt-1").unwrap());
        ids.insert(EventId::parse("evt-2").unwrap());

        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn adapter_id_is_not_agent_id() {
        fn accepts_agent_id(id: AgentId) -> String {
            id.to_string()
        }

        let agent = AgentId::parse("agent-root").unwrap();
        let adapter = AdapterId::parse("adapter-main").unwrap();

        assert_eq!(accepts_agent_id(agent), "agent-root");
        assert_eq!(adapter.as_str(), "adapter-main");
    }
}
