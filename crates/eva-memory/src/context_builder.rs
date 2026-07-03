//! Request-level context assembly from memory and knowledge services.

use crate::knowledge_service::{InMemoryKnowledgeService, KnowledgeSearch, KnowledgeSearchResult};
use crate::memory_service::{InMemoryMemoryService, MemoryRecord};
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
        }
    }

    pub fn with_budget(mut self, budget: ContextBudget) -> Self {
        self.budget = budget;
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
        let memory = self.memory.snapshot_for_agent(
            &request.agent_id,
            request.budget.private_memory,
            request.budget.global_memory,
        );
        let knowledge = self.knowledge.search(
            &KnowledgeSearch::new(request.query.clone()).with_limit(request.budget.knowledge),
        )?;
        let audit = vec![
            format!("private_memory:{}", memory.private.len()),
            format!("global_memory:{}", memory.global.len()),
            format!("knowledge:{}", knowledge.len()),
            "scope:agent_private_plus_global_plus_knowledge".to_owned(),
        ];

        Ok(BuiltContext {
            request_id: request.request_id,
            agent_id: request.agent_id,
            query: request.query,
            memory: memory.private,
            global_memory: memory.global,
            knowledge,
            audit,
        })
    }
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
}
