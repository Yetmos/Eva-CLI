//! 中文：EventBus、scheduler 与 runtime 共享的事件数据契约。
//! English: Event data contracts shared by the EventBus, scheduler, and runtimes.

use crate::capability::CapabilityName;
use crate::ids::{AdapterId, AgentId, EventId, GenerationId, RequestId};
use crate::topic::Topic;
use std::time::SystemTime;

/// 中文：不解释 schema 的不透明事件 payload。
/// English: Opaque event payload carried without schema interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum EventPayload {
    /// 中文：显式空 payload。
    /// English: Explicit empty payload.
    #[default]
    Empty,
    /// 中文：UTF-8 文本 payload。
    /// English: UTF-8 text payload.
    Text(
        /// 按 UTF-8 保存且不在契约层解释 schema 的文本内容。
        String,
    ),
    /// 中文：二进制 payload。
    /// English: Binary payload.
    Bytes(
        /// 不在契约层解释格式的原始字节内容。
        Vec<u8>,
    ),
}

impl EventPayload {
    /// 中文：创建空 payload。
    /// English: Creates an empty payload.
    pub fn empty() -> Self {
        Self::Empty
    }

    /// 中文：创建文本 payload。
    /// English: Creates a text payload.
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }

    /// 中文：创建二进制 payload。
    /// English: Creates a binary payload.
    pub fn bytes(value: impl Into<Vec<u8>>) -> Self {
        Self::Bytes(value.into())
    }

    /// 中文：payload 没有携带文本或字节内容时返回 true。
    /// English: Returns true when the payload carries no bytes or text.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::Text(value) => value.is_empty(),
            Self::Bytes(value) => value.is_empty(),
        }
    }

    /// 中文：当 payload 是文本时返回文本内容。
    /// English: Returns the text payload, when this payload is text.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value.as_str()),
            Self::Empty | Self::Bytes(_) => None,
        }
    }

    /// 中文：当 payload 是二进制时返回字节内容。
    /// English: Returns the byte payload, when this payload is binary.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(value) => Some(value.as_slice()),
            Self::Empty | Self::Text(_) => None,
        }
    }
}

impl From<String> for EventPayload {
    /// 将 owned UTF-8 文本包装为事件 payload，不做业务 schema 解释。
    fn from(value: String) -> Self {
        Self::text(value)
    }
}

impl From<&str> for EventPayload {
    /// 复制借用文本并包装为事件 payload。
    fn from(value: &str) -> Self {
        Self::text(value)
    }
}

impl From<Vec<u8>> for EventPayload {
    /// 接管二进制缓冲区作为不透明事件 payload。
    fn from(value: Vec<u8>) -> Self {
        Self::bytes(value)
    }
}

impl From<&[u8]> for EventPayload {
    /// 复制借用字节切片并包装为不透明事件 payload。
    fn from(value: &[u8]) -> Self {
        Self::bytes(value.to_vec())
    }
}

/// 中文：期望的事件投递目标。
/// English: Desired event delivery target.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum EventTarget {
    /// 中文：没有固定目标；scheduler 可以按 topic 选择接收者。
    /// English: No fixed target; scheduler may select recipients by topic.
    #[default]
    Broadcast,
    /// 中文：将事件定向给某个 Agent。
    /// English: Direct the event to an Agent.
    Agent(
        /// 接收事件的 Agent 稳定标识。
        AgentId,
    ),
    /// 中文：将事件定向给某个 Capability。
    /// English: Direct the event to a Capability.
    Capability(
        /// 接收事件的 Capability 名称。
        CapabilityName,
    ),
    /// 中文：将事件定向给某个 Adapter。
    /// English: Direct the event to an Adapter.
    Adapter(
        /// 接收事件的 Adapter 稳定标识。
        AdapterId,
    ),
}

impl EventTarget {
    /// 中文：事件有显式非广播目标时返回 true。
    /// English: Returns true when the event has an explicit non-broadcast target.
    pub fn is_directed(&self) -> bool {
        !matches!(self, Self::Broadcast)
    }
}

/// 中文：事件链路中的 correlation 与 causation 关联信息。
/// English: Correlation and causation linkage for event chains.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct TraceContext {
    // 中文：correlation_id 指向一条事件链的根，用于跨多跳聚合。
    // English: correlation_id points at the root of an event chain for multi-hop aggregation.
    correlation_id: Option<EventId>,
    // 中文：causation_id 指向直接父事件，用于重建因果边。
    // English: causation_id points at the direct parent event for causality reconstruction.
    causation_id: Option<EventId>,
}

impl TraceContext {
    /// 中文：创建 trace context。
    /// English: Creates a trace context.
    pub fn new(correlation_id: Option<EventId>, causation_id: Option<EventId>) -> Self {
        Self {
            correlation_id,
            causation_id,
        }
    }

    /// 中文：创建带 correlation id 的 context。
    /// English: Creates a context with a correlation id.
    pub fn correlated(correlation_id: EventId) -> Self {
        Self::new(Some(correlation_id), None)
    }

    /// 中文：存在时返回根 correlation id。
    /// English: Returns the root correlation id, when present.
    pub fn correlation_id(&self) -> Option<&EventId> {
        self.correlation_id.as_ref()
    }

    /// 中文：存在时返回直接 causation id。
    /// English: Returns the direct causation id, when present.
    pub fn causation_id(&self) -> Option<&EventId> {
        self.causation_id.as_ref()
    }

    /// 中文：创建子 trace，使 causation 指向 `parent_event_id`。
    /// English: Returns a child trace where causation points at `parent_event_id`.
    pub fn child_of(&self, parent_event_id: EventId) -> Self {
        Self {
            // 中文：若父 trace 没有 correlation，则以父事件作为新链路根。
            // English: If the parent trace lacks correlation, use the parent event as the new chain root.
            correlation_id: self
                .correlation_id
                .clone()
                .or(Some(parent_event_id.clone())),
            causation_id: Some(parent_event_id),
        }
    }
}

/// 中文：非业务事件 metadata。
/// English: Non-business event metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventMetadata {
    // 中文：创建时间用于排序与审计；不是业务 payload 的一部分。
    // English: Creation time is for ordering and audit, not part of the business payload.
    created_at: SystemTime,
    // 中文：request_id 将事件与一次 invoke/CLI 请求关联。
    // English: request_id links the event to an invoke or CLI request.
    request_id: Option<RequestId>,
    // 中文：trace 保存跨事件链路的关联信息。
    // English: trace stores cross-event-chain linkage.
    trace: TraceContext,
    // 中文：generation_id 标识热更新/生命周期代际，帮助避免旧代事件污染新代状态。
    // English: generation_id marks hot-reload/lifecycle generations to prevent old-generation events from contaminating new state.
    generation_id: Option<GenerationId>,
}

impl Default for EventMetadata {
    /// 使用当前时间和空链路信息创建默认 metadata；调用方可随后覆盖各可选字段。
    fn default() -> Self {
        Self {
            created_at: SystemTime::now(),
            request_id: None,
            trace: TraceContext::default(),
            generation_id: None,
        }
    }
}

impl EventMetadata {
    /// 中文：使用当前系统时间创建 metadata。
    /// English: Creates metadata with the current system timestamp.
    pub fn new() -> Self {
        Self::default()
    }

    /// 中文：返回创建时间。
    /// English: Returns the creation timestamp.
    pub fn created_at(&self) -> SystemTime {
        self.created_at
    }

    /// 中文：存在时返回 request id。
    /// English: Returns the request id, when present.
    pub fn request_id(&self) -> Option<&RequestId> {
        self.request_id.as_ref()
    }

    /// 中文：返回 trace context。
    /// English: Returns the trace context.
    pub fn trace(&self) -> &TraceContext {
        &self.trace
    }

    /// 中文：存在时返回 generation id。
    /// English: Returns the generation id, when present.
    pub fn generation_id(&self) -> Option<&GenerationId> {
        self.generation_id.as_ref()
    }

    /// 中文：覆盖创建时间，主要用于测试或导入外部事件。
    /// English: Overrides the creation timestamp, mainly for tests or imported external events.
    pub fn with_created_at(mut self, created_at: SystemTime) -> Self {
        self.created_at = created_at;
        self
    }

    /// 中文：设置 request id。
    /// English: Sets the request id.
    pub fn with_request_id(mut self, request_id: RequestId) -> Self {
        self.request_id = Some(request_id);
        self
    }

    /// 中文：设置 trace context。
    /// English: Sets the trace context.
    pub fn with_trace(mut self, trace: TraceContext) -> Self {
        self.trace = trace;
        self
    }

    /// 中文：设置 generation id。
    /// English: Sets the generation id.
    pub fn with_generation_id(mut self, generation_id: GenerationId) -> Self {
        self.generation_id = Some(generation_id);
        self
    }
}

/// 中文：Eva 模块间传递的标准事件值。
/// English: Standard event value passed between Eva modules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    // 中文：事件唯一标识由上层分配，eva-core 只校验与承载。
    // English: Event IDs are allocated by upper layers; eva-core only validates and carries them.
    event_id: EventId,
    // 中文：topic 用于订阅匹配和路由，不表达具体接收者。
    // English: Topic is used for subscription matching and routing, not for naming a concrete recipient.
    topic: Topic,
    // 中文：target 可覆盖默认广播语义，表达定向投递。
    // English: Target can override default broadcast semantics for directed delivery.
    target: EventTarget,
    // 中文：payload 保持不透明，schema 解析属于业务或 adapter 层。
    // English: Payload stays opaque; schema interpretation belongs to business or adapter layers.
    payload: EventPayload,
    // 中文：metadata 存放链路、时间、请求和代际等非业务信息。
    // English: Metadata stores non-business linkage, timing, request, and generation data.
    metadata: EventMetadata,
}

impl Event {
    /// 中文：使用默认 metadata 创建广播事件。
    /// English: Creates a broadcast event with default metadata.
    pub fn new(event_id: EventId, topic: Topic, payload: EventPayload) -> Self {
        Self {
            event_id,
            topic,
            target: EventTarget::Broadcast,
            payload,
            metadata: EventMetadata::new(),
        }
    }

    /// 中文：返回 event id。
    /// English: Returns the event id.
    pub fn event_id(&self) -> &EventId {
        &self.event_id
    }

    /// 中文：返回 topic。
    /// English: Returns the topic.
    pub fn topic(&self) -> &Topic {
        &self.topic
    }

    /// 中文：返回事件目标。
    /// English: Returns the event target.
    pub fn target(&self) -> &EventTarget {
        &self.target
    }

    /// 中文：返回不透明 payload。
    /// English: Returns the opaque payload.
    pub fn payload(&self) -> &EventPayload {
        &self.payload
    }

    /// 中文：返回 metadata。
    /// English: Returns metadata.
    pub fn metadata(&self) -> &EventMetadata {
        &self.metadata
    }

    /// 中文：设置显式事件目标。
    /// English: Sets an explicit event target.
    pub fn with_target(mut self, target: EventTarget) -> Self {
        self.target = target;
        self
    }

    /// 中文：在 metadata 上设置 request id。
    /// English: Sets the request id on metadata.
    pub fn with_request_id(mut self, request_id: RequestId) -> Self {
        self.metadata = self.metadata.with_request_id(request_id);
        self
    }

    /// 中文：在 metadata 上设置 trace context。
    /// English: Sets trace context on metadata.
    pub fn with_trace(mut self, trace: TraceContext) -> Self {
        self.metadata = self.metadata.with_trace(trace);
        self
    }

    /// 中文：在 metadata 上设置 generation id。
    /// English: Sets generation id on metadata.
    pub fn with_generation_id(mut self, generation_id: GenerationId) -> Self {
        self.metadata = self.metadata.with_generation_id(generation_id);
        self
    }

    /// 中文：整体替换 metadata。
    /// English: Replaces metadata.
    pub fn with_metadata(mut self, metadata: EventMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// 中文：创建子事件，并让 trace 回指当前事件。
    /// English: Creates a child event whose trace points back to this event.
    pub fn child_event(&self, event_id: EventId, topic: Topic, payload: EventPayload) -> Self {
        let trace = self.metadata.trace().child_of(self.event_id.clone());
        Self::new(event_id, topic, payload).with_trace(trace)
    }
}

#[cfg(test)]
/// Event 目标、payload、metadata 和因果链路的回归测试。
mod tests {
    use super::*;

    /// 将测试文本转换为已校验事件 ID，减少各用例的样板代码。
    fn event_id(value: &str) -> EventId {
        EventId::parse(value).unwrap()
    }

    /// 将测试路径转换为已校验 topic。
    fn topic(value: &str) -> Topic {
        Topic::parse(value).unwrap()
    }

    #[test]
    /// 验证构造事件所需的 ID、topic 与 payload 均可稳定读取。
    fn event_requires_id_topic_payload() {
        let event = Event::new(
            event_id("evt-1"),
            topic("/input/user"),
            EventPayload::text("hello"),
        );

        assert_eq!(event.event_id().as_str(), "evt-1");
        assert_eq!(event.topic().as_str(), "/input/user");
        assert_eq!(event.payload().as_text(), Some("hello"));
    }

    #[test]
    /// 验证未指定目标时事件采用非定向广播语义。
    fn event_accepts_broadcast_target() {
        let event = Event::new(event_id("evt-1"), topic("/a/b"), EventPayload::empty());
        assert_eq!(event.target(), &EventTarget::Broadcast);
        assert!(!event.target().is_directed());
    }

    #[test]
    /// 验证 Agent 定向目标会被识别为显式投递。
    fn event_accepts_agent_target() {
        let agent_id = AgentId::parse("agent-root").unwrap();
        let event = Event::new(event_id("evt-1"), topic("/a/b"), EventPayload::empty())
            .with_target(EventTarget::Agent(agent_id.clone()));

        assert_eq!(event.target(), &EventTarget::Agent(agent_id));
        assert!(event.target().is_directed());
    }

    #[test]
    /// 验证 capability 与 adapter 两类定向目标均受契约支持。
    fn event_accepts_capability_and_adapter_targets() {
        let capability = CapabilityName::parse("repo.summary").unwrap();
        let adapter = AdapterId::parse("adapter-cli").unwrap();

        assert!(EventTarget::Capability(capability).is_directed());
        assert!(EventTarget::Adapter(adapter).is_directed());
    }

    #[test]
    /// 验证派生子事件继承已有 correlation 根，避免链路被重新分组。
    fn child_event_preserves_correlation() {
        let root = Event::new(event_id("evt-root"), topic("/root"), EventPayload::empty())
            .with_trace(TraceContext::correlated(event_id("evt-correlation")));
        let child = root.child_event(
            event_id("evt-child"),
            topic("/child"),
            EventPayload::empty(),
        );

        assert_eq!(
            child.metadata().trace().correlation_id().unwrap().as_str(),
            "evt-correlation"
        );
    }

    #[test]
    /// 验证没有 correlation 时以父事件为链路根，并记录直接因果关系。
    fn child_event_sets_causation_to_parent() {
        let parent = Event::new(
            event_id("evt-parent"),
            topic("/root"),
            EventPayload::empty(),
        );
        let child = parent.child_event(
            event_id("evt-child"),
            topic("/child"),
            EventPayload::empty(),
        );

        assert_eq!(
            child.metadata().trace().correlation_id().unwrap().as_str(),
            "evt-parent"
        );
        assert_eq!(
            child.metadata().trace().causation_id().unwrap().as_str(),
            "evt-parent"
        );
    }

    #[test]
    /// 验证请求和运行时代际信息保存在非业务 metadata 中。
    fn event_metadata_tracks_request_and_generation() {
        let request_id = RequestId::parse("req-1").unwrap();
        let generation_id = GenerationId::parse("gen-1").unwrap();
        let event = Event::new(event_id("evt-1"), topic("/a/b"), EventPayload::empty())
            .with_request_id(request_id.clone())
            .with_generation_id(generation_id.clone());

        assert_eq!(event.metadata().request_id(), Some(&request_id));
        assert_eq!(event.metadata().generation_id(), Some(&generation_id));
    }

    #[test]
    /// 验证文本与字节 payload 的便捷转换保持各自类型。
    fn payload_can_be_text_or_bytes() {
        assert_eq!(EventPayload::from("hello").as_text(), Some("hello"));
        assert_eq!(EventPayload::from(vec![1, 2]).as_bytes(), Some(&[1, 2][..]));
    }
}
