//! 中文：Eva 模块间共享的强类型 identifier。
//! English: Strongly typed identifiers shared across Eva modules.

use crate::error::EvaError;
use std::fmt;
use std::str::FromStr;

/// 跨协议标识符的统一字节长度上限，限制状态文件与日志的异常放大风险。
const MAX_ID_LEN: usize = 128;

// 中文：所有 ID 类型共享同一套校验规则，但保持不同 Rust 类型，防止把 AgentId/AdapterId 混用。
// English: All ID types share validation rules while remaining distinct Rust types to prevent AgentId/AdapterId mixups.
macro_rules! define_id {
    ($name:ident, $label:literal) => {
        #[doc = concat!("中文：稳定的 ", $label, " identifier。")]
        #[doc = concat!("English: Stable ", $label, " identifier.")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(
            #[doc = concat!("中文：已通过统一规则校验的 ", $label, " identifier 原始值。")]
            String,
        );

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
            #[doc = concat!("中文：按原值展示 ", $label, " identifier，保证协议与日志表示稳定。")]
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            #[doc = concat!("中文：字符串解析失败时返回结构化 Eva 参数错误。")]
            type Err = EvaError;

            #[doc = concat!("中文：复用公共解析器创建 ", $label, " identifier，不允许 trait 入口绕过校验。")]
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }

        impl TryFrom<&str> for $name {
            #[doc = concat!("中文：借用字符串转换时使用的结构化错误类型。")]
            type Error = EvaError;

            #[doc = concat!("中文：校验借用字符串并转换为 ", $label, " identifier。")]
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

/// 对所有强类型 ID 执行一致的边界校验，保证配置、路径和协议层使用同一字符集。
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

/// 判断字符是否属于跨平台稳定的 ID 字符集。
fn is_stable_id_char(value: char) -> bool {
    value.is_ascii_alphanumeric() || matches!(value, '.' | '_' | '-' | ':')
}

/// 构造包含 ID 类型和值上下文的参数错误，便于调用方定位失败字段。
fn id_error(label: &str, value: &str, message: &str) -> EvaError {
    EvaError::invalid_argument(message)
        .with_context("id_type", label)
        .with_context("id", value)
}

#[cfg(test)]
/// 强类型 ID 校验、展示、哈希和类型隔离的回归测试。
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    /// 验证 Agent ID 接受稳定的短横线 slug。
    fn agent_id_accepts_stable_slug() {
        let id = AgentId::parse("agent-root").unwrap();
        assert_eq!(id.as_str(), "agent-root");
        assert_eq!(id.to_string(), "agent-root");
    }

    #[test]
    /// 验证空 Agent ID 在进入运行时前即被拒绝。
    fn agent_id_rejects_empty() {
        let error = AgentId::parse("").unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::InvalidArgument);
    }

    #[test]
    /// 验证 ID 不允许空白字符，避免 CLI 与配置解析结果分歧。
    fn agent_id_rejects_whitespace() {
        assert!(AgentId::parse("agent root").is_err());
        assert!(AgentId::parse(" agent-root").is_err());
    }

    #[test]
    /// 验证 ID 不能被解释为路径片段。
    fn agent_id_rejects_path_separator() {
        assert!(AgentId::parse("agent/root").is_err());
        assert!(AgentId::parse("agent\\root").is_err());
    }

    #[test]
    /// 验证请求 ID 的展示形式保持输入值不变。
    fn request_id_display_is_stable() {
        let id: RequestId = "req-001".parse().unwrap();
        assert_eq!(id.to_string(), "req-001");
    }

    #[test]
    /// 验证全部强类型 ID 共享相同的合法字符契约。
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
    /// 验证强类型 ID 可安全作为哈希键使用。
    fn ids_can_be_hash_keys() {
        let mut ids = HashSet::new();
        ids.insert(EventId::parse("evt-1").unwrap());
        ids.insert(EventId::parse("evt-1").unwrap());
        ids.insert(EventId::parse("evt-2").unwrap());

        assert_eq!(ids.len(), 2);
    }

    #[test]
    /// 验证不同 ID 新类型在编译期不可互换，即使底层文本相同。
    fn adapter_id_is_not_agent_id() {
        /// 仅接受 AgentId 的测试辅助函数，用于锁定类型隔离契约。
        fn accepts_agent_id(id: AgentId) -> String {
            id.to_string()
        }

        let agent = AgentId::parse("agent-root").unwrap();
        let adapter = AdapterId::parse("adapter-main").unwrap();

        assert_eq!(accepts_agent_id(agent), "agent-root");
        assert_eq!(adapter.as_str(), "adapter-main");
    }
}
