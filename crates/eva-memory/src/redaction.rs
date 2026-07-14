//! 中文：记忆和知识进入上下文前的敏感数据脱敏。
//! Sensitive data redaction before context injection.

use crate::knowledge_service::{KnowledgeItem, KnowledgeSearchResult, KnowledgeSource};
use crate::memory_service::MemoryRecord;
use eva_policy::RedactionPolicyDomain;

/// 中文：本模块在不修改原记录的前提下替换常见凭据令牌并统计替换次数。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "redact sensitive memory and knowledge before context use";

/// 中文：一次文本脱敏的输出和替换计数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedText {
    /// 中文：应用策略后的文本。
    pub value: String,
    /// 中文：被替换的敏感令牌数量。
    pub replacement_count: usize,
}

/// 中文：使用默认策略脱敏一段文本。
pub fn redact_sensitive_text(input: &str) -> RedactedText {
    redact_sensitive_text_with_policy(input, &RedactionPolicyDomain::default())
}

/// 中文：使用指定策略按空白分隔令牌执行脱敏，并保持令牌原始顺序。
///
/// 关闭策略时原样返回；启用时只识别完整令牌中的敏感前缀或 `key=value`、`key:value`
/// 形式，避免把普通正文中的子串任意替换。重新连接会规范化空白，因此该函数用于上下文
/// 文本而非需要字节级保真的原始制品。
pub fn redact_sensitive_text_with_policy(
    input: &str,
    policy: &RedactionPolicyDomain,
) -> RedactedText {
    if !policy.enabled {
        return RedactedText {
            value: input.to_owned(),
            replacement_count: 0,
        };
    }
    let mut replacement_count = 0;
    let value = input
        .split_whitespace()
        .map(|token| {
            let (redacted, count) = redact_token(token, policy);
            replacement_count += count;
            redacted
        })
        .collect::<Vec<_>>()
        .join(" ");
    RedactedText {
        value,
        replacement_count,
    }
}

/// 中文：使用默认策略克隆并脱敏一条记忆记录的值。
pub fn redact_memory_record(record: &MemoryRecord) -> (MemoryRecord, usize) {
    redact_memory_record_with_policy(record, &RedactionPolicyDomain::default())
}

/// 中文：使用指定策略克隆记忆记录，仅替换值字段并返回替换数量。
pub fn redact_memory_record_with_policy(
    record: &MemoryRecord,
    policy: &RedactionPolicyDomain,
) -> (MemoryRecord, usize) {
    let redacted = redact_sensitive_text_with_policy(&record.value, policy);
    let mut record = record.clone();
    record.value = redacted.value;
    (record, redacted.replacement_count)
}

/// 中文：使用默认策略脱敏知识搜索结果中的摘要和正文。
pub fn redact_knowledge_result(result: &KnowledgeSearchResult) -> (KnowledgeSearchResult, usize) {
    redact_knowledge_result_with_policy(result, &RedactionPolicyDomain::default())
}

/// 中文：克隆搜索结果，脱敏摘要与正文并合计替换次数。
pub fn redact_knowledge_result_with_policy(
    result: &KnowledgeSearchResult,
    policy: &RedactionPolicyDomain,
) -> (KnowledgeSearchResult, usize) {
    let summary = redact_sensitive_text_with_policy(&result.item.summary, policy);
    let content = redact_sensitive_text_with_policy(&result.item.content, policy);
    let mut result = result.clone();
    result.item.summary = summary.value;
    result.item.content = content.value;
    (
        result,
        summary.replacement_count + content.replacement_count,
    )
}

/// 中文：使用默认策略脱敏完整知识条目。
pub fn redact_knowledge_item(item: &KnowledgeItem) -> (KnowledgeItem, usize) {
    redact_knowledge_item_with_policy(item, &RedactionPolicyDomain::default())
}

/// 中文：脱敏知识来源 URI、标题、摘要和正文，并重新计算来源内容指纹。
///
/// `KnowledgeSource::new` 使用已脱敏正文重建来源，确保后续索引和缓存不会继续携带由
/// 原始敏感内容计算的标识；返回计数覆盖所有四个文本表面。
pub fn redact_knowledge_item_with_policy(
    item: &KnowledgeItem,
    policy: &RedactionPolicyDomain,
) -> (KnowledgeItem, usize) {
    let source_uri = redact_sensitive_text_with_policy(&item.source.uri, policy);
    let source_title = redact_sensitive_text_with_policy(&item.source.title, policy);
    let summary = redact_sensitive_text_with_policy(&item.summary, policy);
    let content = redact_sensitive_text_with_policy(&item.content, policy);
    let mut item = item.clone();
    item.source = KnowledgeSource::new(
        source_uri.value,
        source_title.value,
        content.value.as_bytes(),
    );
    item.summary = summary.value;
    item.content = content.value;
    (
        item,
        source_uri.replacement_count
            + source_title.replacement_count
            + summary.replacement_count
            + content.replacement_count,
    )
}

/// 中文：按令牌前缀或敏感键值形式判断单个令牌，并最多计为一次替换。
fn redact_token(token: &str, policy: &RedactionPolicyDomain) -> (String, usize) {
    let lower = token.to_ascii_lowercase();
    if policy
        .sensitive_token_prefixes
        .iter()
        .any(|prefix| lower.starts_with(prefix))
    {
        return (policy.replacement.clone(), 1);
    }
    if let Some(index) = token.find('=') {
        let key = &token[..index];
        if is_sensitive_key(key, policy) {
            return (format!("{key}={}", policy.replacement), 1);
        }
    }
    if let Some(index) = token.find(':') {
        let key = &token[..index];
        if is_sensitive_key(key, policy) {
            return (format!("{key}:{}", policy.replacement), 1);
        }
    }
    (token.to_owned(), 0)
}

/// 中文：清理键名外围标点、统一小写后检查是否包含任一敏感片段。
fn is_sensitive_key(key: &str, policy: &RedactionPolicyDomain) -> bool {
    let key = key
        .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-')
        .to_ascii_lowercase();
    policy
        .sensitive_key_fragments
        .iter()
        .any(|fragment| key.contains(fragment))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// 中文：验证等号、冒号和令牌前缀三种敏感形式均被替换。
    fn redact_sensitive_key_value_tokens() {
        let redacted = redact_sensitive_text("token=abc keep password:secret sk-live");

        assert_eq!(
            redacted.value,
            "token=[REDACTED] keep password:[REDACTED] [REDACTED]"
        );
        assert_eq!(redacted.replacement_count, 3);
    }

    #[test]
    /// 中文：验证策略可扩展敏感键、前缀和替换文本。
    fn redaction_policy_can_extend_keys_and_prefixes() {
        let mut policy = RedactionPolicyDomain {
            replacement: "[MASKED]".to_owned(),
            ..RedactionPolicyDomain::default()
        };
        policy
            .sensitive_key_fragments
            .insert("credential".to_owned());
        policy.sensitive_token_prefixes.insert("pk-".to_owned());

        let redacted = redact_sensitive_text_with_policy(
            "credential=abc pk-provider keep token=legacy",
            &policy,
        );

        assert_eq!(
            redacted.value,
            "credential=[MASKED] [MASKED] keep token=[MASKED]"
        );
        assert_eq!(redacted.replacement_count, 3);
    }

    #[test]
    /// 中文：验证关闭脱敏时文本和替换计数保持不变。
    fn disabled_redaction_policy_leaves_text_unchanged() {
        let policy = RedactionPolicyDomain {
            enabled: false,
            ..RedactionPolicyDomain::default()
        };

        let redacted = redact_sensitive_text_with_policy("token=abc sk-live", &policy);

        assert_eq!(redacted.value, "token=abc sk-live");
        assert_eq!(redacted.replacement_count, 0);
    }
}
