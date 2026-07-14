//! Knowledge 条目索引、排序检索与外部 Provider 获取契约。
//! Knowledge item indexing and retrieval contracts.

use crate::observability::{record_memory_observation, MemoryObservation, MemoryOperation};
use crate::redaction::{redact_knowledge_item, redact_knowledge_item_with_policy};
use eva_capability::CapabilityHostApi;
use eva_core::{
    AdapterId, AgentId, CapabilityName, EvaError, InvokeInput, InvokeMetadata, InvokeRequest,
    InvokeStatus, InvokeTarget, RequestId,
};
use eva_observability::{AuditSink, MetricSink, TraceFields};
use eva_policy::{
    HighRiskAction, PolicyDecision, RedactionPolicyDomain, RuntimePolicyGate, RuntimePolicyRequest,
};
use std::collections::BTreeMap;

/// 本模块的架构职责：保存项目知识，并在策略、来源指纹和脱敏门禁后索引外部结果。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "project knowledge storage and retrieval";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// 可作为索引键和磁盘文件名组成部分的稳定 Knowledge 标识。
pub struct KnowledgeId(
    /// 通过非空、首尾空白和路径分隔符校验后的原始稳定 slug。
    String,
);

#[derive(Debug, Clone, PartialEq, Eq)]
/// Knowledge 内容的来源与轻量指纹。
pub struct KnowledgeSource {
    /// 来源 URI 或本地逻辑位置。
    pub uri: String,
    /// 面向检索结果的来源标题。
    pub title: String,
    /// 根据原始内容字节计算的确定性轻量指纹。
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 可索引、可持久化的 Knowledge 条目。
pub struct KnowledgeItem {
    /// 条目稳定标识。
    pub id: KnowledgeId,
    /// 来源元数据和内容指纹。
    pub source: KnowledgeSource,
    /// 高权重检索摘要。
    pub summary: String,
    /// 完整内容。
    pub content: String,
    /// 排序去重后的分类标签。
    pub tags: Vec<String>,
    /// 可选来源请求标识。
    pub request_id: Option<RequestId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Knowledge 检索条件与观察上下文。
pub struct KnowledgeSearch {
    /// 不区分 ASCII 大小写的子串查询。
    pub query: String,
    /// 返回结果上限。
    pub limit: usize,
    /// 结果必须同时包含的全部标签。
    pub required_tags: Vec<String>,
    /// 可选请求追踪标识。
    pub request_id: Option<RequestId>,
    /// 可选发起检索的 Agent。
    pub agent_id: Option<AgentId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 带确定性分数和命中字段说明的检索结果。
pub struct KnowledgeSearchResult {
    /// 命中的完整条目。
    pub item: KnowledgeItem,
    /// 按字段权重累加的相关性分数。
    pub score: usize,
    /// 参与计分的字段或标签。
    pub matched_by: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 通过受策略控制的 Adapter Provider 获取外部知识的请求。
pub struct ExternalKnowledgeRetrievalRequest {
    /// 调用和索引关联的请求标识。
    pub request_id: RequestId,
    /// 发起高风险调用的 Agent。
    pub agent: AgentId,
    /// 要调用的 Capability。
    pub capability: CapabilityName,
    /// 显式 Provider Adapter。
    pub provider: AdapterId,
    /// 发送给 Provider 的非空查询。
    pub query: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 外部 Knowledge 获取是否调用、校验、脱敏并索引的审计报告。
pub struct ExternalKnowledgeRetrievalReport {
    /// indexed、policy_denied、provider_failed/timeout 或 schema_rejected。
    pub status: String,
    /// 请求标识字符串。
    pub request_id: String,
    /// Capability 名称。
    pub capability: String,
    /// Provider Adapter 标识。
    pub provider: String,
    /// 查询字节长度，仅用于诊断而不回显内容。
    pub query_len: usize,
    /// Provider 调用状态。
    pub invocation_status: String,
    /// 成功加入索引的条目数。
    pub items_indexed: usize,
    /// 成功加入索引的条目标识。
    pub indexed_ids: Vec<String>,
    /// 入库前替换的敏感片段数量。
    pub redaction_count: usize,
    /// Provider、URI、来源指纹和脱敏计数审计。
    pub source_audit: Vec<String>,
    /// 策略、调用、Schema 和索引阶段审计。
    pub audit: Vec<String>,
    /// 失败时的稳定错误类别。
    pub error_kind: Option<String>,
    /// 失败时不含结构化上下文的错误消息。
    pub error_message: Option<String>,
}

/// Provider 输出使用的行式 Schema 格式。
const KNOWLEDGE_RETRIEVAL_OUTPUT_FORMAT: &str = "eva.knowledge.retrieval.v1";
/// 发送给 Provider 的查询载荷格式。
const KNOWLEDGE_RETRIEVAL_QUERY_FORMAT: &str = "eva.knowledge.query.v1";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
/// 按 KnowledgeId 有序索引条目的进程内服务。
pub struct InMemoryKnowledgeService {
    /// 稳定标识到唯一条目的确定性映射。
    items: BTreeMap<KnowledgeId, KnowledgeItem>,
}

impl KnowledgeId {
    /// 校验标识非空、已裁剪且不含路径分隔符。
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

    /// 返回原始稳定标识。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl KnowledgeSource {
    /// 创建来源并从内容字节计算确定性轻量指纹。
    ///
    /// 指纹用于变更检测和审计，不是抗碰撞安全摘要，不能替代工件完整性校验。
    pub fn new(uri: impl Into<String>, title: impl Into<String>, bytes: &[u8]) -> Self {
        Self {
            uri: uri.into(),
            title: title.into(),
            digest: lightweight_digest(bytes),
        }
    }
}

impl KnowledgeItem {
    /// 创建摘要和内容均非空的无标签条目。
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

    /// 添加非空且未重复标签，并排序保证持久化和检索确定性。
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        let tag = tag.into();
        if !tag.trim().is_empty() && !self.tags.contains(&tag) {
            self.tags.push(tag);
            self.tags.sort();
        }
        self
    }

    /// 关联来源请求标识。
    pub fn with_request_id(mut self, request_id: RequestId) -> Self {
        self.request_id = Some(request_id);
        self
    }
}

impl KnowledgeSearch {
    /// 创建默认最多返回八项的查询。
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            limit: 8,
            required_tags: Vec::new(),
            request_id: None,
            agent_id: None,
        }
    }

    /// 设置结果上限；零值会得到空结果。
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// 添加一个必须同时存在的标签条件。
    pub fn with_required_tag(mut self, tag: impl Into<String>) -> Self {
        self.required_tags.push(tag.into());
        self
    }

    /// 关联请求追踪标识。
    pub fn with_request_id(mut self, request_id: RequestId) -> Self {
        self.request_id = Some(request_id);
        self
    }

    /// 关联发起检索的 Agent。
    pub fn with_agent_id(mut self, agent_id: AgentId) -> Self {
        self.agent_id = Some(agent_id);
        self
    }
}

impl InMemoryKnowledgeService {
    /// 创建空 Knowledge 索引。
    pub fn new() -> Self {
        Self::default()
    }

    /// 插入唯一条目；重复标识返回冲突且不覆盖原内容。
    pub fn index(&mut self, item: KnowledgeItem) -> Result<(), EvaError> {
        if self.items.contains_key(&item.id) {
            return Err(EvaError::conflict("knowledge item already exists")
                .with_context("knowledge_id", item.id.as_str()));
        }
        self.items.insert(item.id.clone(), item);
        Ok(())
    }

    /// 按标识返回条目副本。
    pub fn get(&self, id: &KnowledgeId) -> Option<KnowledgeItem> {
        self.items.get(id).cloned()
    }

    /// 按全部标签过滤并执行确定性加权子串检索。
    ///
    /// query 统一转为 ASCII 小写；id/title/summary 每项命中加 3，tag 加 2，content 加 1。
    /// 结果先按分数降序，再按 KnowledgeId 升序打破平局，最后截断 limit，因此相同索引
    /// 和查询在不同运行中顺序稳定。
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

    /// 检索后记录查询长度、结果数和可选 Agent/请求；观察失败不修改索引。
    pub fn search_observed<S>(
        &self,
        search: &KnowledgeSearch,
        sink: &mut S,
        trace: &TraceFields,
    ) -> Result<Vec<KnowledgeSearchResult>, EvaError>
    where
        S: AuditSink + MetricSink,
    {
        let results = self.search(search)?;
        let mut observation = MemoryObservation::new(MemoryOperation::Search, trace.clone())
            .with_query_len(search.query.len())
            .with_item_count(results.len());
        if let Some(agent_id) = &search.agent_id {
            observation = observation.with_agent_id(agent_id.clone());
        }
        if let Some(request_id) = &search.request_id {
            observation = observation.with_request_id(request_id.clone());
        }
        record_memory_observation(sink, observation)?;
        Ok(results)
    }

    /// 按 KnowledgeId 顺序返回可用于持久化重建的条目副本。
    pub fn snapshot_items(&self) -> Vec<KnowledgeItem> {
        self.items.values().cloned().collect()
    }

    /// 从条目集合重建索引，重复标识使整个构造失败。
    pub fn rebuild_from_items(items: Vec<KnowledgeItem>) -> Result<Self, EvaError> {
        let mut service = Self::new();
        for item in items {
            service.index(item)?;
        }
        Ok(service)
    }

    /// 返回索引条目数。
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// 判断索引是否为空。
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

impl ExternalKnowledgeRetrievalRequest {
    /// 创建绑定 Agent、Capability、Provider 和查询的外部获取请求。
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

    /// 为 AdapterInvoke 高风险动作计算运行时策略决策。
    pub fn policy_decision(&self, gate: &RuntimePolicyGate) -> PolicyDecision {
        gate.decide(
            RuntimePolicyRequest::new(HighRiskAction::AdapterInvoke)
                .with_agent(self.agent.clone())
                .with_capability(self.capability.clone())
                .with_provider(self.provider.clone())
                .with_adapter(self.provider.clone()),
        )
    }

    /// 使用默认脱敏策略执行外部获取。
    pub fn execute(
        &self,
        gate: &RuntimePolicyGate,
        host: &impl CapabilityHostApi,
        knowledge: &mut InMemoryKnowledgeService,
    ) -> Result<ExternalKnowledgeRetrievalReport, EvaError> {
        self.execute_with_redaction_policy(gate, host, knowledge, &RedactionPolicyDomain::default())
    }

    /// 按策略、Provider、Schema、脱敏、索引的严格顺序执行获取。
    ///
    /// 策略拒绝时不调用 Provider；调用错误、非成功状态、非文本输出或 Schema 错误均
    /// 返回 items_indexed=0 的报告，不污染索引。只有完整解析后才脱敏并计算来源审计，
    /// 最后一次性调用 index；重复 id 冲突作为错误传播，也不会覆盖已有知识。
    pub fn execute_with_redaction_policy(
        &self,
        gate: &RuntimePolicyGate,
        host: &impl CapabilityHostApi,
        knowledge: &mut InMemoryKnowledgeService,
        redaction_policy: &RedactionPolicyDomain,
    ) -> Result<ExternalKnowledgeRetrievalReport, EvaError> {
        if self.query.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "external knowledge retrieval query cannot be empty",
            ));
        }
        // 高风险策略必须先于任何外部 Provider 副作用。
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
        // Provider 失败和业务失败都转换为可审计跳过报告，禁止解析或索引部分输出。
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
        // Schema 完整接受后再脱敏；未经脱敏的外部文本永不进入索引。
        let (item, redaction_count) = if *redaction_policy == RedactionPolicyDomain::default() {
            redact_knowledge_item(&item)
        } else {
            redact_knowledge_item_with_policy(&item, redaction_policy)
        };
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

    /// 将查询、Agent 和请求标识十六进制编码为稳定行式载荷。
    pub fn query_payload(&self) -> String {
        format!(
            "format={KNOWLEDGE_RETRIEVAL_QUERY_FORMAT}\nquery={}\nagent={}\nrequest_id={}\n",
            encode_field(&self.query),
            encode_field(self.agent.as_str()),
            encode_field(self.request_id.as_str())
        )
    }

    /// 构造未索引任何条目的失败或拒绝报告。
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

/// 将 Knowledge 条目序列化为 Provider 输出格式。
///
/// 所有自由文本按 UTF-8 字节十六进制编码，标签可重复键表达，避免换行或 `=` 破坏
/// 行式边界。
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

/// 严格解析 Provider 输出，并用实际 content 重算来源指纹。
///
/// 不信任 Provider 传入 digest；解析器从解码后的 content 构造 KnowledgeSource。格式、
/// 必填字段、十六进制或 UTF-8 任一无效都会阻止索引。
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

/// 解析允许重复键的行式字段映射，供多标签表示使用。
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

/// 读取必填字段的第一个值。
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

/// 将 UTF-8 文本按字节编码为小写十六进制。
fn encode_field(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join("")
}

/// 严格十六进制解码并验证 UTF-8。
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

/// 将调用状态映射为稳定报告字符串。
fn invoke_status(status: InvokeStatus) -> &'static str {
    match status {
        InvokeStatus::Accepted => "accepted",
        InvokeStatus::Completed => "completed",
        InvokeStatus::Failed => "failed",
        InvokeStatus::Cancelled => "cancelled",
        InvokeStatus::Timeout => "timeout",
    }
}

/// 要求条目包含查询声明的全部标签。
fn has_required_tags(item: &KnowledgeItem, required_tags: &[String]) -> bool {
    required_tags.iter().all(|tag| item.tags.contains(tag))
}

/// 计算字段加权子串分数；零分条目不进入结果集。
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

/// 计算长度与字节和组成的轻量来源指纹。
fn lightweight_digest(bytes: &[u8]) -> String {
    let sum = bytes
        .iter()
        .fold(0u64, |accumulator, byte| accumulator + u64::from(*byte));
    format!("len:{}:sum:{}", bytes.len(), sum)
}

#[cfg(test)]
/// Knowledge 排序、重建及外部获取失败隔离测试。
mod tests {
    use super::*;
    use eva_core::{InvokeOutput, InvokeResponse};
    use eva_policy::PolicyDomainSet;

    /// 构造固定来源的测试 Knowledge 条目。
    fn item(id: &str, summary: &str, content: &str) -> KnowledgeItem {
        KnowledgeItem::new(
            KnowledgeId::parse(id).unwrap(),
            KnowledgeSource::new(format!("docs/{id}.md"), id, content.as_bytes()),
            summary,
            content,
        )
        .unwrap()
    }

    /// 解析测试请求标识。
    fn request_id(value: &str) -> RequestId {
        RequestId::parse(value).unwrap()
    }

    /// 解析测试 Agent 标识。
    fn agent(value: &str) -> AgentId {
        AgentId::parse(value).unwrap()
    }

    /// 解析测试 Capability 名称。
    fn capability(value: &str) -> CapabilityName {
        CapabilityName::parse(value).unwrap()
    }

    /// 解析测试 Adapter 标识。
    fn adapter(value: &str) -> AdapterId {
        AdapterId::parse(value).unwrap()
    }

    #[derive(Debug, Clone)]
    /// 断言显式 Provider 和请求上下文并返回预设响应的测试 Host。
    struct FakeRetrievalHost {
        /// 调用必须指定的 Provider。
        expected_provider: AdapterId,
        /// Provider 应返回的响应。
        response: InvokeResponse,
    }

    impl CapabilityHostApi for FakeRetrievalHost {
        /// 使用预期 Provider 委托调用。
        fn invoke(&self, request: InvokeRequest) -> Result<InvokeResponse, EvaError> {
            self.invoke_with_provider(request, Some(self.expected_provider.clone()))
        }

        /// 验证 Provider、目标、查询格式和 caller 后返回响应。
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

    /// 构造固定外部 Knowledge 获取请求。
    fn retrieval_request() -> ExternalKnowledgeRetrievalRequest {
        ExternalKnowledgeRetrievalRequest::new(
            request_id("req-knowledge-1"),
            agent("root-agent"),
            capability("knowledge.retrieve"),
            adapter("retrieval-provider"),
            "runtime memory",
        )
    }

    /// 用预设响应构造测试 Host。
    fn retrieval_host(response: InvokeResponse) -> FakeRetrievalHost {
        FakeRetrievalHost {
            expected_provider: adapter("retrieval-provider"),
            response,
        }
    }

    #[test]
    /// 验证查询、标签过滤和相关性排序。
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
    /// 验证重复 KnowledgeId 不会覆盖已有条目。
    fn duplicate_ids_are_rejected() {
        let mut service = InMemoryKnowledgeService::new();
        service.index(item("memory-plan", "one", "body")).unwrap();
        let error = service
            .index(item("memory-plan", "two", "body"))
            .unwrap_err();
        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }

    #[test]
    /// 验证索引可从持久条目集合确定性重建。
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
    /// 验证外部获取必须经过运行时策略门禁。
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
    /// 验证 Provider 结果脱敏后索引并记录来源指纹。
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
    /// 验证自定义策略在索引前驱动脱敏。
    fn external_retrieval_uses_policy_driven_redaction_before_indexing() {
        let mut knowledge = InMemoryKnowledgeService::new();
        let request = retrieval_request();
        let item = item(
            "retrieved-policy-memory",
            "runtime credential=secret summary",
            "runtime memory pk-provider body token=legacy",
        )
        .with_tag("retrieval");
        let host = retrieval_host(InvokeResponse::completed(
            request.request_id.clone(),
            InvokeOutput::text(render_retrieval_item(&item)),
        ));
        let mut policy = RedactionPolicyDomain {
            replacement: "[MASKED]".to_owned(),
            ..RedactionPolicyDomain::default()
        };
        policy
            .sensitive_key_fragments
            .insert("credential".to_owned());
        policy.sensitive_token_prefixes.insert("pk-".to_owned());

        let report = request
            .execute_with_redaction_policy(
                &RuntimePolicyGate::new(PolicyDomainSet::default()),
                &host,
                &mut knowledge,
                &policy,
            )
            .unwrap();
        let indexed = knowledge
            .get(&KnowledgeId::parse("retrieved-policy-memory").unwrap())
            .unwrap();

        assert_eq!(report.status, "indexed");
        assert_eq!(report.redaction_count, 3);
        assert_eq!(indexed.summary, "runtime credential=[MASKED] summary");
        assert_eq!(
            indexed.content,
            "runtime memory [MASKED] body token=[MASKED]"
        );
    }

    #[test]
    /// 验证 Schema 无效的 Provider 输出不会污染索引。
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
    /// 验证 Provider 超时不会污染索引。
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
    /// 验证 Provider 失败响应不会污染索引。
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
    /// 验证策略拒绝会跳过 Provider 调用和索引。
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
