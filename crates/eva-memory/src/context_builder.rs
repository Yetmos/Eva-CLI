//! Request-level context assembly from memory and knowledge services.

use crate::knowledge_service::{InMemoryKnowledgeService, KnowledgeSearch, KnowledgeSearchResult};
use crate::memory_service::{InMemoryMemoryService, MemoryRecord};
use crate::observability::{record_memory_observation, MemoryObservation, MemoryOperation};
use crate::redaction::{redact_knowledge_result_with_policy, redact_memory_record_with_policy};
use eva_core::{AgentId, EvaError, RequestId};
use eva_observability::{AuditSink, MetricSink, TraceFields};
use eva_policy::RedactionPolicyDomain;

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
    pub redaction_count: usize,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ContextBuilder<'a> {
    memory: &'a InMemoryMemoryService,
    knowledge: &'a InMemoryKnowledgeService,
    redaction_policy: RedactionPolicyDomain,
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
        Self {
            memory,
            knowledge,
            redaction_policy: RedactionPolicyDomain::default(),
        }
    }

    pub fn with_redaction_policy(mut self, policy: RedactionPolicyDomain) -> Self {
        self.redaction_policy = policy;
        self
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
        let (private_memory, private_redactions) =
            redact_memory_records(memory.private, &self.redaction_policy);
        let (global_memory, global_redactions) =
            redact_memory_records(memory.global, &self.redaction_policy);
        let (knowledge, knowledge_redactions) =
            redact_knowledge_results(knowledge, &self.redaction_policy);
        let redaction_count = private_redactions + global_redactions + knowledge_redactions;
        let mut audit = vec![
            format!("private_memory:{}", private_memory.len()),
            format!("global_memory:{}", global_memory.len()),
            format!("knowledge:{}", knowledge.len()),
            format!("expiration_reference_ms:{}", request.now_ms),
            "scope:agent_private_plus_global_plus_knowledge".to_owned(),
        ];
        if self.redaction_policy.audit_redactions {
            audit.push(format!("redaction:{redaction_count}"));
        } else {
            audit.push("redaction_audit:disabled".to_owned());
        }

        Ok(BuiltContext {
            request_id: request.request_id,
            agent_id: request.agent_id,
            query: request.query,
            memory: private_memory,
            global_memory,
            knowledge,
            redaction_count,
            audit,
        })
    }

    pub fn build_observed<S>(
        &self,
        request: ContextRequest,
        sink: &mut S,
        trace: &TraceFields,
    ) -> Result<BuiltContext, EvaError>
    where
        S: AuditSink + MetricSink,
    {
        if request.query.trim().is_empty() {
            return Err(EvaError::invalid_argument("context query cannot be empty"));
        }
        let request_id = request.request_id.clone();
        let agent_id = request.agent_id.clone();
        let memory = self.memory.snapshot_for_agent_at(
            &request.agent_id,
            request.budget.private_memory,
            request.budget.global_memory,
            request.now_ms,
        );
        record_memory_observation(
            sink,
            MemoryObservation::new(MemoryOperation::Read, trace.clone())
                .with_request_id(request_id.clone())
                .with_agent_id(agent_id.clone())
                .with_item_count(memory.private.len() + memory.global.len()),
        )?;
        let knowledge_search = KnowledgeSearch::new(request.query.clone())
            .with_limit(request.budget.knowledge)
            .with_request_id(request_id.clone())
            .with_agent_id(agent_id.clone());
        let knowledge = self
            .knowledge
            .search_observed(&knowledge_search, sink, trace)?;
        let (private_memory, private_redactions) =
            redact_memory_records(memory.private, &self.redaction_policy);
        let (global_memory, global_redactions) =
            redact_memory_records(memory.global, &self.redaction_policy);
        let (knowledge, knowledge_redactions) =
            redact_knowledge_results(knowledge, &self.redaction_policy);
        let redaction_count = private_redactions + global_redactions + knowledge_redactions;
        let mut audit = vec![
            format!("private_memory:{}", private_memory.len()),
            format!("global_memory:{}", global_memory.len()),
            format!("knowledge:{}", knowledge.len()),
            format!("expiration_reference_ms:{}", request.now_ms),
            "scope:agent_private_plus_global_plus_knowledge".to_owned(),
        ];
        if self.redaction_policy.audit_redactions {
            audit.push(format!("redaction:{redaction_count}"));
        } else {
            audit.push("redaction_audit:disabled".to_owned());
        }

        let context = BuiltContext {
            request_id,
            agent_id,
            query: request.query,
            memory: private_memory,
            global_memory,
            knowledge,
            redaction_count,
            audit,
        };
        record_memory_observation(
            sink,
            MemoryObservation::new(MemoryOperation::Context, trace.clone())
                .with_request_id(context.request_id.clone())
                .with_agent_id(context.agent_id.clone())
                .with_item_count(context.total_items())
                .with_redaction_count(context.redaction_count),
        )?;
        Ok(context)
    }
}

fn redact_memory_records(
    records: Vec<MemoryRecord>,
    policy: &RedactionPolicyDomain,
) -> (Vec<MemoryRecord>, usize) {
    let mut replacement_count = 0;
    let records = records
        .into_iter()
        .map(|record| {
            let (record, count) = redact_memory_record_with_policy(&record, policy);
            replacement_count += count;
            record
        })
        .collect();
    (records, replacement_count)
}

fn redact_knowledge_results(
    results: Vec<KnowledgeSearchResult>,
    policy: &RedactionPolicyDomain,
) -> (Vec<KnowledgeSearchResult>, usize) {
    let mut replacement_count = 0;
    let results = results
        .into_iter()
        .map(|result| {
            let (result, count) = redact_knowledge_result_with_policy(&result, policy);
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
    use eva_observability::{
        AuditEvent, AuditSink, InMemoryAuditSink, InMemoryMetricSink, MetricPoint, MetricSink,
    };

    #[derive(Debug, Default)]
    struct TestSink {
        audit: InMemoryAuditSink,
        metrics: InMemoryMetricSink,
    }

    impl AuditSink for TestSink {
        fn record(&mut self, event: AuditEvent) -> Result<(), EvaError> {
            self.audit.record(event)
        }
    }

    impl MetricSink for TestSink {
        fn record(&mut self, point: MetricPoint) -> Result<(), EvaError> {
            self.metrics.record(point)
        }
    }

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
        assert_eq!(built.redaction_count, 1);
        assert!(built.audit.contains(&"redaction:1".to_owned()));
    }

    #[test]
    fn context_uses_policy_driven_redaction_rules() {
        let mut memory = InMemoryMemoryService::new();
        let root = agent("root-agent");
        memory
            .write(MemoryWrite::private(
                root.clone(),
                "provider.credential",
                "credential=abc pk-provider token=legacy",
            ))
            .unwrap();
        let knowledge = InMemoryKnowledgeService::new();
        let mut policy = RedactionPolicyDomain {
            replacement: "[MASKED]".to_owned(),
            ..RedactionPolicyDomain::default()
        };
        policy
            .sensitive_key_fragments
            .insert("credential".to_owned());
        policy.sensitive_token_prefixes.insert("pk-".to_owned());

        let built = ContextBuilder::new(&memory, &knowledge)
            .with_redaction_policy(policy)
            .build(ContextRequest::new(
                request("req-memory-policy"),
                root,
                "credential",
            ))
            .unwrap();

        assert_eq!(
            built.memory[0].value,
            "credential=[MASKED] [MASKED] token=[MASKED]"
        );
        assert_eq!(built.redaction_count, 3);
        assert!(built.audit.contains(&"redaction:3".to_owned()));
        assert!(!built.lua_summary().audit.join(";").contains("abc"));
    }

    #[test]
    fn observed_context_records_read_search_and_context_events() {
        let mut memory = InMemoryMemoryService::new();
        let root = agent("root-agent");
        let request_id = request("req-memory-observed-context");
        memory
            .write(
                MemoryWrite::private(root.clone(), "live", "token=secret memory")
                    .with_request_id(request_id.clone()),
            )
            .unwrap();
        let mut knowledge = InMemoryKnowledgeService::new();
        knowledge
            .index(
                KnowledgeItem::new(
                    KnowledgeId::parse("memory-doc").unwrap(),
                    KnowledgeSource::new("docs/memory.md", "memory", b"memory token=secret"),
                    "Memory token=secret",
                    "memory context",
                )
                .unwrap(),
            )
            .unwrap();
        let mut sink = TestSink::default();

        let built = ContextBuilder::new(&memory, &knowledge)
            .build_observed(
                ContextRequest::new(request_id.clone(), root.clone(), "memory"),
                &mut sink,
                &TraceFields::default(),
            )
            .unwrap();

        let actions = sink
            .audit
            .events
            .iter()
            .map(|event| event.action.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            actions,
            vec!["memory.read", "memory.search", "memory.context"]
        );
        assert_eq!(
            sink.audit.events[0].trace.request_id.as_ref(),
            Some(&request_id)
        );
        assert_eq!(sink.audit.events[0].trace.agent_id.as_ref(), Some(&root));
        assert_eq!(sink.metrics.points.len(), 6);
        assert_eq!(built.redaction_count, 2);
        assert_eq!(built.lua_summary().private_memory_count, 1);
        assert!(!built.lua_summary().audit.join(";").contains("secret"));
    }
}
