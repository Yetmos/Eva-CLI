//! Sensitive data redaction before context injection.

use crate::knowledge_service::{KnowledgeItem, KnowledgeSearchResult, KnowledgeSource};
use crate::memory_service::MemoryRecord;
use eva_policy::RedactionPolicyDomain;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "redact sensitive memory and knowledge before context use";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedText {
    pub value: String,
    pub replacement_count: usize,
}

pub fn redact_sensitive_text(input: &str) -> RedactedText {
    redact_sensitive_text_with_policy(input, &RedactionPolicyDomain::default())
}

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

pub fn redact_memory_record(record: &MemoryRecord) -> (MemoryRecord, usize) {
    redact_memory_record_with_policy(record, &RedactionPolicyDomain::default())
}

pub fn redact_memory_record_with_policy(
    record: &MemoryRecord,
    policy: &RedactionPolicyDomain,
) -> (MemoryRecord, usize) {
    let redacted = redact_sensitive_text_with_policy(&record.value, policy);
    let mut record = record.clone();
    record.value = redacted.value;
    (record, redacted.replacement_count)
}

pub fn redact_knowledge_result(result: &KnowledgeSearchResult) -> (KnowledgeSearchResult, usize) {
    redact_knowledge_result_with_policy(result, &RedactionPolicyDomain::default())
}

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

pub fn redact_knowledge_item(item: &KnowledgeItem) -> (KnowledgeItem, usize) {
    redact_knowledge_item_with_policy(item, &RedactionPolicyDomain::default())
}

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
    fn redact_sensitive_key_value_tokens() {
        let redacted = redact_sensitive_text("token=abc keep password:secret sk-live");

        assert_eq!(
            redacted.value,
            "token=[REDACTED] keep password:[REDACTED] [REDACTED]"
        );
        assert_eq!(redacted.replacement_count, 3);
    }

    #[test]
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
