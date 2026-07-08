//! Knowledge item indexing and retrieval contracts.

use eva_core::{AdapterId, AgentId, CapabilityName, EvaError, RequestId};
use eva_policy::{HighRiskAction, PolicyDecision, RuntimePolicyGate, RuntimePolicyRequest};
use std::collections::BTreeMap;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "project knowledge storage and retrieval";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KnowledgeId(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeSource {
    pub uri: String,
    pub title: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeItem {
    pub id: KnowledgeId,
    pub source: KnowledgeSource,
    pub summary: String,
    pub content: String,
    pub tags: Vec<String>,
    pub request_id: Option<RequestId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeSearch {
    pub query: String,
    pub limit: usize,
    pub required_tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeSearchResult {
    pub item: KnowledgeItem,
    pub score: usize,
    pub matched_by: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalKnowledgeRetrievalRequest {
    pub agent: AgentId,
    pub capability: CapabilityName,
    pub provider: AdapterId,
    pub query: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryKnowledgeService {
    items: BTreeMap<KnowledgeId, KnowledgeItem>,
}

impl KnowledgeId {
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        if value.trim().is_empty() {
            return Err(EvaError::invalid_argument("knowledge id cannot be empty"));
        }
        if value.trim() != value || value.contains('/') || value.contains('\\') {
            return Err(EvaError::invalid_argument(
                "knowledge id must be a stable slug",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl KnowledgeSource {
    pub fn new(uri: impl Into<String>, title: impl Into<String>, bytes: &[u8]) -> Self {
        Self {
            uri: uri.into(),
            title: title.into(),
            digest: lightweight_digest(bytes),
        }
    }
}

impl KnowledgeItem {
    pub fn new(
        id: KnowledgeId,
        source: KnowledgeSource,
        summary: impl Into<String>,
        content: impl Into<String>,
    ) -> Result<Self, EvaError> {
        let summary = summary.into();
        let content = content.into();
        if summary.trim().is_empty() || content.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "knowledge summary and content are required",
            ));
        }
        Ok(Self {
            id,
            source,
            summary,
            content,
            tags: Vec::new(),
            request_id: None,
        })
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        let tag = tag.into();
        if !tag.trim().is_empty() && !self.tags.contains(&tag) {
            self.tags.push(tag);
            self.tags.sort();
        }
        self
    }

    pub fn with_request_id(mut self, request_id: RequestId) -> Self {
        self.request_id = Some(request_id);
        self
    }
}

impl KnowledgeSearch {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            limit: 8,
            required_tags: Vec::new(),
        }
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    pub fn with_required_tag(mut self, tag: impl Into<String>) -> Self {
        self.required_tags.push(tag.into());
        self
    }
}

impl InMemoryKnowledgeService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn index(&mut self, item: KnowledgeItem) -> Result<(), EvaError> {
        if self.items.contains_key(&item.id) {
            return Err(EvaError::conflict("knowledge item already exists")
                .with_context("knowledge_id", item.id.as_str()));
        }
        self.items.insert(item.id.clone(), item);
        Ok(())
    }

    pub fn get(&self, id: &KnowledgeId) -> Option<KnowledgeItem> {
        self.items.get(id).cloned()
    }

    pub fn search(&self, search: &KnowledgeSearch) -> Result<Vec<KnowledgeSearchResult>, EvaError> {
        if search.query.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "knowledge search query cannot be empty",
            ));
        }
        let query = search.query.to_ascii_lowercase();
        let mut results = self
            .items
            .values()
            .filter(|item| has_required_tags(item, &search.required_tags))
            .filter_map(|item| score_item(item, &query))
            .collect::<Vec<_>>();
        results.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then(left.item.id.cmp(&right.item.id))
        });
        results.truncate(search.limit);
        Ok(results)
    }

    pub fn snapshot_items(&self) -> Vec<KnowledgeItem> {
        self.items.values().cloned().collect()
    }

    pub fn rebuild_from_items(items: Vec<KnowledgeItem>) -> Result<Self, EvaError> {
        let mut service = Self::new();
        for item in items {
            service.index(item)?;
        }
        Ok(service)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

impl ExternalKnowledgeRetrievalRequest {
    pub fn new(
        agent: AgentId,
        capability: CapabilityName,
        provider: AdapterId,
        query: impl Into<String>,
    ) -> Self {
        Self {
            agent,
            capability,
            provider,
            query: query.into(),
        }
    }

    pub fn policy_decision(&self, gate: &RuntimePolicyGate) -> PolicyDecision {
        gate.decide(
            RuntimePolicyRequest::new(HighRiskAction::AdapterInvoke)
                .with_agent(self.agent.clone())
                .with_capability(self.capability.clone())
                .with_provider(self.provider.clone())
                .with_adapter(self.provider.clone()),
        )
    }
}

fn has_required_tags(item: &KnowledgeItem, required_tags: &[String]) -> bool {
    required_tags.iter().all(|tag| item.tags.contains(tag))
}

fn score_item(item: &KnowledgeItem, query: &str) -> Option<KnowledgeSearchResult> {
    let mut score = 0;
    let mut matched_by = Vec::new();
    let haystacks = [
        ("id", item.id.as_str()),
        ("title", item.source.title.as_str()),
        ("summary", item.summary.as_str()),
        ("content", item.content.as_str()),
    ];
    for (field, value) in haystacks {
        if value.to_ascii_lowercase().contains(query) {
            score += if field == "content" { 1 } else { 3 };
            matched_by.push(field.to_owned());
        }
    }
    for tag in &item.tags {
        if tag.to_ascii_lowercase().contains(query) {
            score += 2;
            matched_by.push(format!("tag:{tag}"));
        }
    }
    (score > 0).then(|| KnowledgeSearchResult {
        item: item.clone(),
        score,
        matched_by,
    })
}

fn lightweight_digest(bytes: &[u8]) -> String {
    let sum = bytes
        .iter()
        .fold(0u64, |accumulator, byte| accumulator + u64::from(*byte));
    format!("len:{}:sum:{}", bytes.len(), sum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_policy::PolicyDomainSet;

    fn item(id: &str, summary: &str, content: &str) -> KnowledgeItem {
        KnowledgeItem::new(
            KnowledgeId::parse(id).unwrap(),
            KnowledgeSource::new(format!("docs/{id}.md"), id, content.as_bytes()),
            summary,
            content,
        )
        .unwrap()
    }

    #[test]
    fn indexes_and_searches_by_query_and_tag() {
        let mut service = InMemoryKnowledgeService::new();
        service
            .index(item("memory-plan", "Memory plan", "ContextBuilder budget").with_tag("v1.2"))
            .unwrap();
        service
            .index(item("hardware-plan", "Hardware plan", "DeviceRegistry").with_tag("v1.3"))
            .unwrap();

        let results = service
            .search(&KnowledgeSearch::new("plan").with_required_tag("v1.2"))
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].item.id.as_str(), "memory-plan");
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let mut service = InMemoryKnowledgeService::new();
        service.index(item("memory-plan", "one", "body")).unwrap();
        let error = service
            .index(item("memory-plan", "two", "body"))
            .unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    fn knowledge_index_can_be_rebuilt_from_items() {
        let mut service = InMemoryKnowledgeService::new();
        service
            .index(item("memory-plan", "one", "memory body"))
            .unwrap();
        service
            .index(item("runtime-plan", "two", "runtime body"))
            .unwrap();

        let rebuilt =
            InMemoryKnowledgeService::rebuild_from_items(service.snapshot_items()).unwrap();
        let results = rebuilt.search(&KnowledgeSearch::new("runtime")).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].item.id.as_str(), "runtime-plan");
    }

    #[test]
    fn external_retrieval_requires_runtime_policy_gate() {
        let mut domains = PolicyDomainSet::default();
        domains
            .adapter
            .deny_capabilities
            .insert(CapabilityName::parse("knowledge.retrieve").unwrap());
        let gate = RuntimePolicyGate::new(domains);
        let request = ExternalKnowledgeRetrievalRequest::new(
            AgentId::parse("root-agent").unwrap(),
            CapabilityName::parse("knowledge.retrieve").unwrap(),
            AdapterId::parse("claude-api").unwrap(),
            "memory",
        );

        let decision = request.policy_decision(&gate);

        assert!(!decision.allowed);
        assert_eq!(decision.action, HighRiskAction::AdapterInvoke);
    }
}
