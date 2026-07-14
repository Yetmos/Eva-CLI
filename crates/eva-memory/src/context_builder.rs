//! 中文：在请求级预算和策略约束下组装记忆与知识上下文。
//! Request-level context assembly from memory and knowledge services.

use crate::knowledge_service::{InMemoryKnowledgeService, KnowledgeSearch, KnowledgeSearchResult};
use crate::memory_service::{InMemoryMemoryService, MemoryRecord};
use crate::observability::{record_memory_observation, MemoryObservation, MemoryOperation};
use crate::redaction::{redact_knowledge_result_with_policy, redact_memory_record_with_policy};
use eva_core::{AgentId, EvaError, RequestId};
use eva_observability::{AuditSink, MetricSink, TraceFields};
use eva_policy::RedactionPolicyDomain;

/// 中文：本模块隔离私有/全局记忆、执行过期过滤和脱敏，并构造可审计上下文。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "policy-aware context assembly";

/// 中文：一次上下文构建允许从各来源取回的最大条目数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBudget {
    /// 中文：当前 Agent 私有记忆的最大条目数。
    pub private_memory: usize,
    /// 中文：全局共享记忆的最大条目数。
    pub global_memory: usize,
    /// 中文：知识检索结果的最大条目数。
    pub knowledge: usize,
}

/// 中文：构建上下文所需的请求主体、查询、预算和过期时间参考。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRequest {
    /// 中文：关联整个上下文调用链的请求标识。
    pub request_id: RequestId,
    /// 中文：决定私有记忆可见范围的 Agent 标识。
    pub agent_id: AgentId,
    /// 中文：用于知识检索的非空查询文本。
    pub query: String,
    /// 中文：三个上下文来源各自的条目上限。
    pub budget: ContextBudget,
    /// 中文：判断 TTL 是否过期的调用方时间毫秒值。
    pub now_ms: u128,
}

/// 中文：完成过期过滤、权限隔离和脱敏后的上下文结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltContext {
    /// 中文：输入请求标识的稳定副本。
    pub request_id: RequestId,
    /// 中文：上下文所属的 Agent。
    pub agent_id: AgentId,
    /// 中文：知识检索使用的原查询文本。
    pub query: String,
    /// 中文：只属于请求 Agent 的私有记忆。
    pub memory: Vec<MemoryRecord>,
    /// 中文：对全部 Agent 可见的全局记忆。
    pub global_memory: Vec<MemoryRecord>,
    /// 中文：按相关性返回的知识检索结果。
    pub knowledge: Vec<KnowledgeSearchResult>,
    /// 中文：三个来源合计执行的敏感值替换次数。
    pub redaction_count: usize,
    /// 中文：不含原始内容的范围、数量、过期参考和脱敏审计摘要。
    pub audit: Vec<String>,
}

/// 中文：借用记忆与知识服务、持有脱敏策略的上下文构建器。
#[derive(Debug, Clone)]
pub struct ContextBuilder<'a> {
    /// 中文：提供按 Agent 与 TTL 过滤快照的记忆服务。
    memory: &'a InMemoryMemoryService,
    /// 中文：提供查询相关性排序的知识服务。
    knowledge: &'a InMemoryKnowledgeService,
    /// 中文：上下文注入前应用的脱敏策略快照。
    redaction_policy: RedactionPolicyDomain,
}

impl Default for ContextBudget {
    /// 中文：默认每个来源最多取八条，限制上下文膨胀。
    fn default() -> Self {
        Self {
            private_memory: 8,
            global_memory: 8,
            knowledge: 8,
        }
    }
}

impl ContextRequest {
    /// 中文：使用默认预算和时间零创建上下文请求。
    pub fn new(request_id: RequestId, agent_id: AgentId, query: impl Into<String>) -> Self {
        Self {
            request_id,
            agent_id,
            query: query.into(),
            budget: ContextBudget::default(),
            now_ms: 0,
        }
    }

    /// 中文：覆盖三个来源的条目预算。
    pub fn with_budget(mut self, budget: ContextBudget) -> Self {
        self.budget = budget;
        self
    }

    /// 中文：设置 TTL 过期判断使用的当前时间毫秒值。
    pub fn with_now_ms(mut self, now_ms: u128) -> Self {
        self.now_ms = now_ms;
        self
    }
}

impl BuiltContext {
    /// 中文：返回私有记忆、全局记忆和知识结果的总条目数。
    pub fn total_items(&self) -> usize {
        self.memory.len() + self.global_memory.len() + self.knowledge.len()
    }

    /// 中文：生成只含计数和审计摘要的 Lua 上下文快照，不暴露原始记忆文本。
    pub fn lua_summary(&self) -> LuaContextSnapshot {
        LuaContextSnapshot {
            private_memory_count: self.memory.len(),
            global_memory_count: self.global_memory.len(),
            knowledge_count: self.knowledge.len(),
            audit: self.audit.clone(),
        }
    }
}

/// 中文：提供给 Lua 宿主的最小上下文统计快照。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LuaContextSnapshot {
    /// 中文：注入的私有记忆条目数。
    pub private_memory_count: usize,
    /// 中文：注入的全局记忆条目数。
    pub global_memory_count: usize,
    /// 中文：注入的知识结果数。
    pub knowledge_count: usize,
    /// 中文：不含原始内容的上下文审计摘要。
    pub audit: Vec<String>,
}

impl<'a> ContextBuilder<'a> {
    /// 中文：借用内存服务并使用默认脱敏策略创建构建器。
    pub fn new(memory: &'a InMemoryMemoryService, knowledge: &'a InMemoryKnowledgeService) -> Self {
        Self {
            memory,
            knowledge,
            redaction_policy: RedactionPolicyDomain::default(),
        }
    }

    /// 中文：覆盖上下文注入前使用的脱敏策略。
    pub fn with_redaction_policy(mut self, policy: RedactionPolicyDomain) -> Self {
        self.redaction_policy = policy;
        self
    }

    /// 中文：构建不写入可观察性后端的策略感知上下文。
    ///
    /// 先拒绝空查询，再按 Agent、预算和 `now_ms` 读取快照；知识检索完成后统一克隆并
    /// 脱敏所有来源。审计只记录数量、范围和时间参考，不包含记忆、查询或秘密原文。
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

    /// 中文：构建上下文并记录记忆读取、知识检索和最终上下文三阶段观测。
    ///
    /// 观测写入是执行契约的一部分：任何审计或指标错误都会使构建失败。读取观测发生在
    /// 检索前，最终上下文观测发生在脱敏后，因此指标中的条目与替换计数对应实际返回值。
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

/// 中文：按输入顺序脱敏一组记忆记录，并累计全部替换次数。
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

/// 中文：按相关性顺序脱敏知识结果，并累计摘要与正文替换次数。
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
    /// 中文：同时收集上下文构建审计和指标的测试写入端。
    struct TestSink {
        /// 中文：内存审计收集器。
        audit: InMemoryAuditSink,
        /// 中文：内存指标收集器。
        metrics: InMemoryMetricSink,
    }

    impl AuditSink for TestSink {
        /// 中文：把审计事件转发到测试收集器。
        fn record(&mut self, event: AuditEvent) -> Result<(), EvaError> {
            self.audit.record(event)
        }
    }

    impl MetricSink for TestSink {
        /// 中文：把指标点转发到测试收集器。
        fn record(&mut self, point: MetricPoint) -> Result<(), EvaError> {
            self.metrics.record(point)
        }
    }

    /// 中文：解析上下文测试使用的 Agent 标识。
    fn agent(value: &str) -> AgentId {
        AgentId::parse(value).unwrap()
    }

    /// 中文：解析上下文测试使用的请求标识。
    fn request(value: &str) -> RequestId {
        RequestId::parse(value).unwrap()
    }

    #[test]
    /// 中文：验证私有记忆只对所属 Agent 可见，同时仍可读取全局记忆和知识。
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
    /// 中文：验证三个来源分别遵守请求预算。
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
    /// 中文：验证过期记忆被过滤且有效记忆在注入前完成脱敏。
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
    /// 中文：验证自定义敏感键、前缀和替换文本应用到上下文且审计不泄密。
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
    /// 中文：验证观测构建按读取、检索、上下文顺序写入审计和对应指标。
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
