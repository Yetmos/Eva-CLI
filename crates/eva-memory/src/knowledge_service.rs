//! Knowledge item indexing and retrieval contracts.

use crate::redaction::redact_knowledge_item;
use eva_capability::CapabilityHostApi;
use eva_core::{
    AdapterId, AgentId, CapabilityName, EvaError, InvokeInput, InvokeMetadata, InvokeRequest,
    InvokeStatus, InvokeTarget, RequestId,
};
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
    pub request_id: RequestId,
    pub agent: AgentId,
    pub capability: CapabilityName,
    pub provider: AdapterId,
    pub query: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalKnowledgeRetrievalReport {
    pub status: String,
    pub request_id: String,
    pub capability: String,
    pub provider: String,
    pub query_len: usize,
    pub invocation_status: String,
    pub items_indexed: usize,
    pub indexed_ids: Vec<String>,
    pub redaction_count: usize,
    pub source_audit: Vec<String>,
    pub audit: Vec<String>,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
}

const KNOWLEDGE_RETRIEVAL_OUTPUT_FORMAT: &str = "eva.knowledge.retrieval.v1";
const KNOWLEDGE_RETRIEVAL_QUERY_FORMAT: &str = "eva.knowledge.query.v1";

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
        request_id: RequestId,
        agent: AgentId,
        capability: CapabilityName,
        provider: AdapterId,
        query: impl Into<String>,
    ) -> Self {
        Self {
            request_id,
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

    pub fn execute(
        &self,
        gate: &RuntimePolicyGate,
        host: &impl CapabilityHostApi,
        knowledge: &mut InMemoryKnowledgeService,
    ) -> Result<ExternalKnowledgeRetrievalReport, EvaError> {
        if self.query.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "external knowledge retrieval query cannot be empty",
            ));
        }
        let decision = self.policy_decision(gate);
        if !decision.allowed {
            return Ok(self.skipped_report(
                "policy_denied",
                "not_invoked",
                decision.audit,
                Some(
                    EvaError::permission_denied("runtime policy denied knowledge retrieval")
                        .with_context("reason", decision.reason),
                ),
            ));
        }

        let request = InvokeRequest::new(
            self.request_id.clone(),
            InvokeTarget::Capability(self.capability.clone()),
            InvokeInput::text(self.query_payload()),
        )
        .with_metadata(InvokeMetadata::new().with_caller(self.agent.clone()));
        let response = match host.invoke_with_provider(request, Some(self.provider.clone())) {
            Ok(response) => response,
            Err(error) => {
                return Ok(self.skipped_report(
                    "provider_failed",
                    "error",
                    vec![
                        "knowledge.retrieval:provider_invoke_error".to_owned(),
                        "knowledge.retrieval:index_skipped".to_owned(),
                    ],
                    Some(error),
                ))
            }
        };
        let invocation_status = invoke_status(response.status()).to_owned();
        if !response.is_success() {
            let status = if response.status() == InvokeStatus::Timeout {
                "provider_timeout"
            } else {
                "provider_failed"
            };
            return Ok(self.skipped_report(
                status,
                &invocation_status,
                vec![
                    format!("knowledge.retrieval:provider_status:{invocation_status}"),
                    "knowledge.retrieval:index_skipped".to_owned(),
                ],
                response.error().cloned(),
            ));
        }

        let Some(output) = response.output().and_then(|output| output.as_text()) else {
            return Ok(self.skipped_report(
                "schema_rejected",
                &invocation_status,
                vec![
                    "knowledge.retrieval:schema_rejected".to_owned(),
                    "knowledge.retrieval:index_skipped".to_owned(),
                ],
                Some(EvaError::invalid_argument(
                    "knowledge retrieval provider returned non-text output",
                )),
            ));
        };
        let item = match parse_retrieval_item(output, &self.request_id) {
            Ok(item) => item,
            Err(error) => {
                return Ok(self.skipped_report(
                    "schema_rejected",
                    &invocation_status,
                    vec![
                        "knowledge.retrieval:schema_rejected".to_owned(),
                        "knowledge.retrieval:index_skipped".to_owned(),
                    ],
                    Some(error),
                ))
            }
        };
        let (item, redaction_count) = redact_knowledge_item(&item);
        let indexed_id = item.id.as_str().to_owned();
        let source_audit = vec![
            format!("source.provider:{}", self.provider.as_str()),
            format!("source.uri:{}", item.source.uri),
            format!("source.digest:{}", item.source.digest),
            format!("source.redaction_count:{redaction_count}"),
        ];
        knowledge.index(item)?;
        Ok(ExternalKnowledgeRetrievalReport {
            status: "indexed".to_owned(),
            request_id: self.request_id.as_str().to_owned(),
            capability: self.capability.as_str().to_owned(),
            provider: self.provider.as_str().to_owned(),
            query_len: self.query.len(),
            invocation_status,
            items_indexed: 1,
            indexed_ids: vec![indexed_id],
            redaction_count,
            source_audit,
            audit: vec![
                "knowledge.retrieval:policy_allowed".to_owned(),
                "knowledge.retrieval:provider_invoked".to_owned(),
                "knowledge.retrieval:schema_accepted".to_owned(),
                "knowledge.retrieval:indexed:1".to_owned(),
            ],
            error_kind: None,
            error_message: None,
        })
    }

    pub fn query_payload(&self) -> String {
        format!(
            "format={KNOWLEDGE_RETRIEVAL_QUERY_FORMAT}\nquery={}\nagent={}\nrequest_id={}\n",
            encode_field(&self.query),
            encode_field(self.agent.as_str()),
            encode_field(self.request_id.as_str())
        )
    }

    fn skipped_report(
        &self,
        status: &str,
        invocation_status: &str,
        audit: Vec<String>,
        error: Option<EvaError>,
    ) -> ExternalKnowledgeRetrievalReport {
        ExternalKnowledgeRetrievalReport {
            status: status.to_owned(),
            request_id: self.request_id.as_str().to_owned(),
            capability: self.capability.as_str().to_owned(),
            provider: self.provider.as_str().to_owned(),
            query_len: self.query.len(),
            invocation_status: invocation_status.to_owned(),
            items_indexed: 0,
            indexed_ids: Vec::new(),
            redaction_count: 0,
            source_audit: Vec::new(),
            audit,
            error_kind: error.as_ref().map(|error| error.kind().as_str().to_owned()),
            error_message: error.map(|error| error.message().to_owned()),
        }
    }
}

pub fn render_retrieval_item(item: &KnowledgeItem) -> String {
    let mut lines = vec![
        format!("format={KNOWLEDGE_RETRIEVAL_OUTPUT_FORMAT}"),
        format!("item_id={}", encode_field(item.id.as_str())),
        format!("source_uri={}", encode_field(&item.source.uri)),
        format!("source_title={}", encode_field(&item.source.title)),
        format!("summary={}", encode_field(&item.summary)),
        format!("content={}", encode_field(&item.content)),
    ];
    for tag in &item.tags {
        lines.push(format!("tag={}", encode_field(tag)));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn parse_retrieval_item(data: &str, request_id: &RequestId) -> Result<KnowledgeItem, EvaError> {
    let fields = parse_multimap(data)?;
    if first_field(&fields, "format")? != KNOWLEDGE_RETRIEVAL_OUTPUT_FORMAT {
        return Err(EvaError::invalid_argument(
            "knowledge retrieval provider returned unsupported schema format",
        ));
    }
    let content = decode_field(first_field(&fields, "content")?)?;
    let mut item = KnowledgeItem::new(
        KnowledgeId::parse(&decode_field(first_field(&fields, "item_id")?)?)?,
        KnowledgeSource::new(
            decode_field(first_field(&fields, "source_uri")?)?,
            decode_field(first_field(&fields, "source_title")?)?,
            content.as_bytes(),
        ),
        decode_field(first_field(&fields, "summary")?)?,
        content,
    )?
    .with_request_id(request_id.clone());
    if let Some(tags) = fields.get("tag") {
        for tag in tags {
            item = item.with_tag(decode_field(tag)?);
        }
    }
    Ok(item)
}

fn parse_multimap(data: &str) -> Result<BTreeMap<String, Vec<String>>, EvaError> {
    let mut fields = BTreeMap::new();
    for line in data.lines().filter(|line| !line.trim().is_empty()) {
        let Some((key, value)) = line.split_once('=') else {
            return Err(EvaError::invalid_argument(
                "knowledge retrieval provider returned invalid schema line",
            ));
        };
        fields
            .entry(key.to_owned())
            .or_insert_with(Vec::new)
            .push(value.to_owned());
    }
    Ok(fields)
}

fn first_field<'a>(
    fields: &'a BTreeMap<String, Vec<String>>,
    key: &str,
) -> Result<&'a str, EvaError> {
    fields
        .get(key)
        .and_then(|values| values.first())
        .map(String::as_str)
        .ok_or_else(|| {
            EvaError::invalid_argument("knowledge retrieval provider output is missing field")
                .with_context("field", key)
        })
}

fn encode_field(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join("")
}

fn decode_field(value: &str) -> Result<String, EvaError> {
    if !value.len().is_multiple_of(2) {
        return Err(EvaError::invalid_argument(
            "knowledge retrieval encoded field length is invalid",
        ));
    }
    let mut bytes = Vec::new();
    for chunk in value.as_bytes().chunks(2) {
        let hex = std::str::from_utf8(chunk).map_err(|_| {
            EvaError::invalid_argument("knowledge retrieval encoded field is not utf8")
        })?;
        bytes.push(u8::from_str_radix(hex, 16).map_err(|_| {
            EvaError::invalid_argument("knowledge retrieval encoded field is not hex")
        })?);
    }
    String::from_utf8(bytes)
        .map_err(|_| EvaError::invalid_argument("knowledge retrieval field is not utf8"))
}

fn invoke_status(status: InvokeStatus) -> &'static str {
    match status {
        InvokeStatus::Accepted => "accepted",
        InvokeStatus::Completed => "completed",
        InvokeStatus::Failed => "failed",
        InvokeStatus::Cancelled => "cancelled",
        InvokeStatus::Timeout => "timeout",
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
    use eva_core::{InvokeOutput, InvokeResponse};
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

    fn request_id(value: &str) -> RequestId {
        RequestId::parse(value).unwrap()
    }

    fn agent(value: &str) -> AgentId {
        AgentId::parse(value).unwrap()
    }

    fn capability(value: &str) -> CapabilityName {
        CapabilityName::parse(value).unwrap()
    }

    fn adapter(value: &str) -> AdapterId {
        AdapterId::parse(value).unwrap()
    }

    #[derive(Debug, Clone)]
    struct FakeRetrievalHost {
        expected_provider: AdapterId,
        response: InvokeResponse,
    }

    impl CapabilityHostApi for FakeRetrievalHost {
        fn invoke(&self, request: InvokeRequest) -> Result<InvokeResponse, EvaError> {
            self.invoke_with_provider(request, Some(self.expected_provider.clone()))
        }

        fn invoke_with_provider(
            &self,
            request: InvokeRequest,
            explicit_provider: Option<AdapterId>,
        ) -> Result<InvokeResponse, EvaError> {
            assert_eq!(explicit_provider.as_ref(), Some(&self.expected_provider));
            assert!(matches!(request.target(), InvokeTarget::Capability(_)));
            assert!(request
                .input()
                .as_text()
                .unwrap()
                .contains(KNOWLEDGE_RETRIEVAL_QUERY_FORMAT));
            assert_eq!(request.metadata().caller().unwrap().as_str(), "root-agent");
            Ok(self.response.clone())
        }
    }

    fn retrieval_request() -> ExternalKnowledgeRetrievalRequest {
        ExternalKnowledgeRetrievalRequest::new(
            request_id("req-knowledge-1"),
            agent("root-agent"),
            capability("knowledge.retrieve"),
            adapter("retrieval-provider"),
            "runtime memory",
        )
    }

    fn retrieval_host(response: InvokeResponse) -> FakeRetrievalHost {
        FakeRetrievalHost {
            expected_provider: adapter("retrieval-provider"),
            response,
        }
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
            request_id("req-knowledge-policy"),
            agent("root-agent"),
            capability("knowledge.retrieve"),
            adapter("claude-api"),
            "memory",
        );

        let decision = request.policy_decision(&gate);

        assert!(!decision.allowed);
        assert_eq!(decision.action, HighRiskAction::AdapterInvoke);
    }

    #[test]
    fn external_retrieval_indexes_redacted_provider_result_with_source_audit() {
        let mut knowledge = InMemoryKnowledgeService::new();
        let request = retrieval_request();
        let item = item(
            "retrieved-runtime-memory",
            "runtime token=secret summary",
            "runtime memory password:secret body",
        )
        .with_tag("retrieval")
        .with_tag("v1.15.7");
        let host = retrieval_host(InvokeResponse::completed(
            request.request_id.clone(),
            InvokeOutput::text(render_retrieval_item(&item)),
        ));

        let report = request
            .execute(
                &RuntimePolicyGate::new(PolicyDomainSet::default()),
                &host,
                &mut knowledge,
            )
            .unwrap();
        let indexed = knowledge
            .get(&KnowledgeId::parse("retrieved-runtime-memory").unwrap())
            .unwrap();

        assert_eq!(report.status, "indexed");
        assert_eq!(report.invocation_status, "completed");
        assert_eq!(report.items_indexed, 1);
        assert_eq!(report.indexed_ids, vec!["retrieved-runtime-memory"]);
        assert_eq!(report.redaction_count, 2);
        assert!(report
            .source_audit
            .contains(&"source.provider:retrieval-provider".to_owned()));
        assert!(report
            .audit
            .contains(&"knowledge.retrieval:schema_accepted".to_owned()));
        assert_eq!(indexed.summary, "runtime token=[REDACTED] summary");
        assert_eq!(indexed.content, "runtime memory password:[REDACTED] body");
        assert_eq!(indexed.request_id.as_ref(), Some(&request.request_id));
        assert_eq!(indexed.tags, vec!["retrieval", "v1.15.7"]);
        assert_eq!(
            indexed.source.digest,
            KnowledgeSource::new("", "", indexed.content.as_bytes()).digest
        );
    }

    #[test]
    fn external_retrieval_schema_rejection_does_not_pollute_index() {
        let mut knowledge = InMemoryKnowledgeService::new();
        let request = retrieval_request();
        let host = retrieval_host(InvokeResponse::completed(
            request.request_id.clone(),
            InvokeOutput::text("format=unknown\n"),
        ));

        let report = request
            .execute(
                &RuntimePolicyGate::new(PolicyDomainSet::default()),
                &host,
                &mut knowledge,
            )
            .unwrap();

        assert_eq!(report.status, "schema_rejected");
        assert_eq!(report.items_indexed, 0);
        assert_eq!(report.error_kind.as_deref(), Some("invalid_argument"));
        assert!(knowledge.is_empty());
    }

    #[test]
    fn external_retrieval_timeout_does_not_pollute_index() {
        let mut knowledge = InMemoryKnowledgeService::new();
        let request = retrieval_request();
        let host = retrieval_host(InvokeResponse::timeout(
            request.request_id.clone(),
            "provider timed out",
        ));

        let report = request
            .execute(
                &RuntimePolicyGate::new(PolicyDomainSet::default()),
                &host,
                &mut knowledge,
            )
            .unwrap();

        assert_eq!(report.status, "provider_timeout");
        assert_eq!(report.invocation_status, "timeout");
        assert_eq!(report.items_indexed, 0);
        assert_eq!(report.error_kind.as_deref(), Some("timeout"));
        assert!(knowledge.is_empty());
    }

    #[test]
    fn external_retrieval_failed_response_does_not_pollute_index() {
        let mut knowledge = InMemoryKnowledgeService::new();
        let request = retrieval_request();
        let host = retrieval_host(InvokeResponse::failed(
            request.request_id.clone(),
            EvaError::unavailable("retrieval provider unavailable"),
        ));

        let report = request
            .execute(
                &RuntimePolicyGate::new(PolicyDomainSet::default()),
                &host,
                &mut knowledge,
            )
            .unwrap();

        assert_eq!(report.status, "provider_failed");
        assert_eq!(report.invocation_status, "failed");
        assert_eq!(report.items_indexed, 0);
        assert_eq!(report.error_kind.as_deref(), Some("unavailable"));
        assert!(knowledge.is_empty());
    }

    #[test]
    fn external_retrieval_policy_denial_skips_provider_and_index() {
        let mut domains = PolicyDomainSet::default();
        domains
            .adapter
            .deny_capabilities
            .insert(capability("knowledge.retrieve"));
        let mut knowledge = InMemoryKnowledgeService::new();
        let request = retrieval_request();
        let host = retrieval_host(InvokeResponse::completed(
            request.request_id.clone(),
            InvokeOutput::text(render_retrieval_item(&item("should-not-index", "x", "y"))),
        ));

        let report = request
            .execute(&RuntimePolicyGate::new(domains), &host, &mut knowledge)
            .unwrap();

        assert_eq!(report.status, "policy_denied");
        assert_eq!(report.invocation_status, "not_invoked");
        assert_eq!(report.items_indexed, 0);
        assert!(report.audit.contains(&"policy.decision:deny".to_owned()));
        assert!(knowledge.is_empty());
    }
}
