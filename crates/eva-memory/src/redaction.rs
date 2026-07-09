//! Sensitive data redaction before context injection.

use crate::knowledge_service::{KnowledgeItem, KnowledgeSearchResult, KnowledgeSource};
use crate::memory_service::MemoryRecord;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "redact sensitive memory and knowledge before context use";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedText {
    pub value: String,
    pub replacement_count: usize,
}

pub fn redact_sensitive_text(input: &str) -> RedactedText {
    let mut replacement_count = 0;
    let value = input
        .split_whitespace()
        .map(|token| {
            let (redacted, count) = redact_token(token);
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
    let redacted = redact_sensitive_text(&record.value);
    let mut record = record.clone();
    record.value = redacted.value;
    (record, redacted.replacement_count)
}

pub fn redact_knowledge_result(result: &KnowledgeSearchResult) -> (KnowledgeSearchResult, usize) {
    let summary = redact_sensitive_text(&result.item.summary);
    let content = redact_sensitive_text(&result.item.content);
    let mut result = result.clone();
    result.item.summary = summary.value;
    result.item.content = content.value;
    (
        result,
        summary.replacement_count + content.replacement_count,
    )
}

pub fn redact_knowledge_item(item: &KnowledgeItem) -> (KnowledgeItem, usize) {
    let source_uri = redact_sensitive_text(&item.source.uri);
    let source_title = redact_sensitive_text(&item.source.title);
    let summary = redact_sensitive_text(&item.summary);
    let content = redact_sensitive_text(&item.content);
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

fn redact_token(token: &str) -> (String, usize) {
    if token.to_ascii_lowercase().starts_with("sk-") {
        return ("[REDACTED]".to_owned(), 1);
    }
    if let Some(index) = token.find('=') {
        let key = &token[..index];
        if is_sensitive_key(key) {
            return (format!("{key}=[REDACTED]"), 1);
        }
    }
    if let Some(index) = token.find(':') {
        let key = &token[..index];
        if is_sensitive_key(key) {
            return (format!("{key}:[REDACTED]"), 1);
        }
    }
    (token.to_owned(), 0)
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key
        .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-')
        .to_ascii_lowercase();
    key.contains("password")
        || key.contains("secret")
        || key.contains("token")
        || key.contains("api_key")
        || key.contains("apikey")
        || key.contains("authorization")
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
}
