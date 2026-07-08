//! Request-level context assembly from memory and knowledge services.

use crate::knowledge_service::{InMemoryKnowledgeService, KnowledgeSearch, KnowledgeSearchResult};
use crate::memory_service::{InMemoryMemoryService, MemoryRecord};
use crate::redaction::{redact_knowledge_result, redact_memory_record};
use eva_core::{AgentId, EvaError, RequestId};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "policy-aware context assembly";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBudget {
    pub private_memory: usize,
    pub global_memory: usize,
    pub knowledge: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRequest {
    pub request_id: RequestId,
    pub agent_id: AgentId,
    pub query: String,
    pub budget: ContextBudget,
    pub now_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltContext {
    pub request_id: RequestId,
    pub agent_id: AgentId,
    pub query: String,
    pub memory: Vec<MemoryRecord>,
    pub global_memory: Vec<MemoryRecord>,
    pub knowledge: Vec<KnowledgeSearchResult>,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct ContextBuilder<'a> {
    memory: &'a InMemoryMemoryService,
    knowledge: &'a InMemoryKnowledgeService,
}

impl Default for ContextBudget {
    fn default() -> Self {
        Self {
            private_memory: 8,
            global_memory: 8,
            knowledge: 8,
        }
    }
}

impl ContextRequest {
    pub fn new(request_id: RequestId, agent_id: AgentId, query: impl Into<String>) -> Self {
        Self {
            request_id,
            agent_id,
            query: query.into(),
            budget: ContextBudget::default(),
            now_ms: 0,
        }
    }

    pub fn with_budget(mut self, budget: ContextBudget) -> Self {
        self.budget = budget;
        self
    }

    pub fn with_now_ms(mut self, now_ms: u128) -> Self {
        self.now_ms = now_ms;
        self
    }
}

impl BuiltContext {
    pub fn total_items(&self) -> usize {
        self.memory.len() + self.global_memory.len() + self.knowledge.len()
    }

    pub fn lua_summary(&self) -> LuaContextSnapshot {
        LuaContextSnapshot {
            private_memory_count: self.memory.len(),
            global_memory_count: self.global_memory.len(),
            knowledge_count: self.knowledge.len(),
            audit: self.audit.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LuaContextSnapshot {
    pub private_memory_count: usize,
    pub global_memory_count: usize,
    pub knowledge_count: usize,
    pub audit: Vec<String>,
}

impl<'a> ContextBuilder<'a> {
    pub fn new(memory: &'a InMemoryMemoryService, knowledge: &'a InMemoryKnowledgeService) -> Self {
        Self { memory, knowledge }
    }

    pub fn build(&self, request: ContextRequest) -> Result<BuiltContext, EvaError> {
        if request.query.trim().is_empty() {
            return Err(EvaError::invalid_argument("context query cannot be empty"));
        }
        let memory = self.memory.snapshot_for_agent_at(
            &request.agent_id,
            request.budget.private_memory,
            request.budget.global_memory,
            request.now_ms,
        );
        let knowledge = self.knowledge.search(
            &KnowledgeSearch::new(request.query.clone()).with_limit(request.budget.knowledge),
        )?;
        let (private_memory, private_redactions) = redact_memory_records(memory.private);
        let (global_memory, global_redactions) = redact_memory_records(memory.global);
        let (knowledge, knowledge_redactions) = redact_knowledge_results(knowledge);
        let redaction_count = private_redactions + global_redactions + knowledge_redactions;
        let audit = vec![
            format!("private_memory:{}", private_memory.len()),
            format!("global_memory:{}", global_memory.len()),
            format!("knowledge:{}", knowledge.len()),
            format!("redaction:{}", redaction_count),
            format!("expiration_reference_ms:{}", request.now_ms),
            "scope:agent_private_plus_global_plus_knowledge".to_owned(),
        ];

        Ok(BuiltContext {
            request_id: request.request_id,
            agent_id: request.agent_id,
            query: request.query,
            memory: private_memory,
            global_memory,
            knowledge,
            audit,
        })
    }
}

fn redact_memory_records(records: Vec<MemoryRecord>) -> (Vec<MemoryRecord>, usize) {
    let mut replacement_count = 0;
    let records = records
        .into_iter()
        .map(|record| {
            let (record, count) = redact_memory_record(&record);
            replacement_count += count;
            record
        })
        .collect();
    (records, replacement_count)
}

fn redact_knowledge_results(
    results: Vec<KnowledgeSearchResult>,
) -> (Vec<KnowledgeSearchResult>, usize) {
    let mut replacement_count = 0;
    let results = results
        .into_iter()
        .map(|result| {
            let (result, count) = redact_knowledge_result(&result);
            replacement_count += count;
            result
        })
        .collect();
    (results, replacement_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge_service::{KnowledgeId, KnowledgeItem, KnowledgeSource};
    use crate::memory_service::MemoryWrite;

    fn agent(value: &str) -> AgentId {
        AgentId::parse(value).unwrap()
    }

    fn request(value: &str) -> RequestId {
        RequestId::parse(value).unwrap()
    }

    #[test]
    fn context_uses_only_requesting_agents_private_memory() {
        let mut memory = InMemoryMemoryService::new();
        let root = agent("root-agent");
        let other = agent("agent-a");
        memory
            .write(MemoryWrite::private(
                root.clone(),
                "goal",
                "ship memory context",
            ))
            .unwrap();
        memory
            .write(MemoryWrite::private(other, "secret", "do not leak"))
            .unwrap();
        memory
            .write(MemoryWrite::global("release", "v1.2"))
            .unwrap();

        let mut knowledge = InMemoryKnowledgeService::new();
        knowledge
            .index(
                KnowledgeItem::new(
                    KnowledgeId::parse("memory-doc").unwrap(),
                    KnowledgeSource::new("docs/memory.md", "memory", b"memory context"),
                    "Memory context",
                    "ContextBuilder reads agent memory safely",
                )
                .unwrap(),
            )
            .unwrap();

        let built = ContextBuilder::new(&memory, &knowledge)
            .build(ContextRequest::new(request("req-memory-1"), root, "memory"))
            .unwrap();

        assert_eq!(built.memory.len(), 1);
        assert_eq!(built.global_memory.len(), 1);
        assert_eq!(built.knowledge.len(), 1);
        assert!(!built.memory.iter().any(|record| record.key == "secret"));
    }

    #[test]
    fn context_respects_item_budgets() {
        let mut memory = InMemoryMemoryService::new();
        let root = agent("root-agent");
        memory
            .write(MemoryWrite::private(root.clone(), "a", "one"))
            .unwrap();
        memory
            .write(MemoryWrite::private(root.clone(), "b", "two"))
            .unwrap();

        let knowledge = InMemoryKnowledgeService::new();
        let budget = ContextBudget {
            private_memory: 1,
            global_memory: 0,
            knowledge: 0,
        };

        let built = ContextBuilder::new(&memory, &knowledge)
            .build(ContextRequest::new(request("req-memory-2"), root, "one").with_budget(budget))
            .unwrap();

        assert_eq!(built.memory.len(), 1);
        assert_eq!(built.global_memory.len(), 0);
        assert_eq!(built.knowledge.len(), 0);
    }

    #[test]
    fn context_filters_expired_memory_and_redacts_sensitive_values() {
        let mut memory = InMemoryMemoryService::new();
        let root = agent("root-agent");
        memory
            .write(MemoryWrite::private(root.clone(), "live", "token=secret").with_ttl_ms(100, 100))
            .unwrap();
        memory
            .write(
                MemoryWrite::private(root.clone(), "expired", "password=secret").with_ttl_ms(1, 1),
            )
            .unwrap();

        let knowledge = InMemoryKnowledgeService::new();
        let built = ContextBuilder::new(&memory, &knowledge)
            .build(ContextRequest::new(request("req-memory-3"), root, "token").with_now_ms(50))
            .unwrap();

        assert_eq!(built.memory.len(), 1);
        assert_eq!(built.memory[0].value, "token=[REDACTED]");
        assert!(built.audit.contains(&"redaction:1".to_owned()));
    }
}
