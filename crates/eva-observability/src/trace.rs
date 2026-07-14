//! 中文：跨 CLI、运行时、Agent 和 Adapter 传播的追踪字段契约。
//! Trace fields and propagation contracts.

use eva_core::{
    AdapterId, AgentId, CapabilityName, Event, EventId, EventTarget, GenerationId, RequestId, Topic,
};
use std::fmt;

/// 中文：可观察性记录携带的稳定 Span 标识。
/// Stable span identifier carried by observability records.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpanId(
    /// 中文：经过稳定字符校验的 Span 标识文本。
    String,
);

/// 中文：运行时、Adapter 和 CLI 记录可共享的统一追踪字段。
/// Common trace fields that all runtime, adapter, and CLI records can share.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TraceFields {
    /// 中文：当前记录关联的事件标识。
    pub event_id: Option<EventId>,
    /// 中文：跨命令和异步任务保持不变的请求标识。
    pub request_id: Option<RequestId>,
    /// 中文：当前事件或操作关联的主题。
    pub topic: Option<Topic>,
    /// 中文：当前处理或目标 Agent。
    pub agent_id: Option<AgentId>,
    /// 中文：当前调用或目标 Adapter。
    pub adapter_id: Option<AdapterId>,
    /// 中文：当前调用的 capability。
    pub capability: Option<CapabilityName>,
    /// 中文：实际执行调用的 Provider 名称。
    pub provider: Option<String>,
    /// 中文：一条业务链中根事件的关联标识。
    pub correlation_id: Option<EventId>,
    /// 中文：直接导致当前事件的父事件标识。
    pub causation_id: Option<EventId>,
    /// 中文：处理当前记录的运行时代际。
    pub generation_id: Option<GenerationId>,
    /// 中文：当前观测操作的 Span 标识。
    pub span_id: Option<SpanId>,
}

impl SpanId {
    /// 中文：校验 Span 标识非空、无边缘空白且只包含跨后端稳定的 ASCII 字符。
    pub fn parse(value: &str) -> Result<Self, eva_core::EvaError> {
        if value.is_empty() || value.trim() != value {
            return Err(eva_core::EvaError::invalid_argument(
                "span id cannot be empty or contain leading/trailing whitespace",
            ));
        }
        if !value
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '.' | '_' | '-' | ':'))
        {
            return Err(eva_core::EvaError::invalid_argument(
                "span id may only contain ASCII letters, digits, '.', '_', '-', and ':'",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    /// 中文：返回经过校验的 Span 标识文本。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SpanId {
    /// 中文：把稳定 Span 标识原样写入格式化器。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TraceFields {
    /// 中文：从核心事件提取标识、主题、目标和元数据，不读取或记录载荷。
    ///
    /// 目标类型决定填充 Agent、capability 或 Adapter 字段；广播目标不增加主体字段，
    /// 从而避免在追踪层猜测实际消费者。
    /// Builds trace fields from a core event without interpreting its payload.
    pub fn from_event(event: &Event) -> Self {
        let mut fields = Self {
            event_id: Some(event.event_id().clone()),
            request_id: event.metadata().request_id().cloned(),
            topic: Some(event.topic().clone()),
            correlation_id: event.metadata().trace().correlation_id().cloned(),
            causation_id: event.metadata().trace().causation_id().cloned(),
            generation_id: event.metadata().generation_id().cloned(),
            ..Self::default()
        };

        match event.target() {
            EventTarget::Broadcast => {}
            EventTarget::Agent(agent_id) => fields.agent_id = Some(agent_id.clone()),
            EventTarget::Capability(capability) => fields.capability = Some(capability.clone()),
            EventTarget::Adapter(adapter_id) => fields.adapter_id = Some(adapter_id.clone()),
        }

        fields
    }

    /// 中文：附加或覆盖 Agent 标识。
    pub fn with_agent_id(mut self, value: AgentId) -> Self {
        self.agent_id = Some(value);
        self
    }

    /// 中文：附加或覆盖请求标识。
    pub fn with_request_id(mut self, value: RequestId) -> Self {
        self.request_id = Some(value);
        self
    }

    /// 中文：附加或覆盖 Adapter 标识。
    pub fn with_adapter_id(mut self, value: AdapterId) -> Self {
        self.adapter_id = Some(value);
        self
    }

    /// 中文：附加或覆盖 capability 名称。
    pub fn with_capability(mut self, value: CapabilityName) -> Self {
        self.capability = Some(value);
        self
    }

    /// 中文：附加或覆盖 Provider 名称。
    pub fn with_provider(mut self, value: impl Into<String>) -> Self {
        self.provider = Some(value.into());
        self
    }

    /// 中文：附加或覆盖当前 Span 标识。
    pub fn with_span_id(mut self, value: SpanId) -> Self {
        self.span_id = Some(value);
        self
    }

    /// 中文：克隆全部链路上下文，仅替换 Span 标识以表示新的子操作。
    pub fn child_span(&self, span_id: SpanId) -> Self {
        let mut child = self.clone();
        child.span_id = Some(span_id);
        child
    }

    /// 中文：按请求、关联事件、Span 的优先级生成跨记录连续性键。
    ///
    /// 优先选择覆盖面最大的请求标识；缺失时退化到关联事件，最后使用局部 Span。
    /// 所有字段都缺失时返回 `None`，调用方不应制造随机连续性。
    pub fn continuity_key(&self) -> Option<String> {
        self.request_id
            .as_ref()
            .map(|value| format!("request_id:{}", value.as_str()))
            .or_else(|| {
                self.correlation_id
                    .as_ref()
                    .map(|value| format!("correlation_id:{}", value.as_str()))
            })
            .or_else(|| {
                self.span_id
                    .as_ref()
                    .map(|value| format!("span_id:{}", value.as_str()))
            })
    }

    /// 中文：按固定字段顺序返回当前存在的值，供文本和 JSON 适配器稳定输出。
    /// Returns a flat list of present fields for text/JSON output adapters.
    pub fn entries(&self) -> Vec<(&'static str, String)> {
        let mut entries = Vec::new();
        push_optional(&mut entries, "event_id", self.event_id.as_ref());
        push_optional(&mut entries, "request_id", self.request_id.as_ref());
        push_optional(&mut entries, "topic", self.topic.as_ref());
        push_optional(&mut entries, "agent_id", self.agent_id.as_ref());
        push_optional(&mut entries, "adapter_id", self.adapter_id.as_ref());
        push_optional(&mut entries, "capability", self.capability.as_ref());
        push_optional_str(&mut entries, "provider", self.provider.as_deref());
        push_optional(&mut entries, "correlation_id", self.correlation_id.as_ref());
        push_optional(&mut entries, "causation_id", self.causation_id.as_ref());
        push_optional(&mut entries, "generation_id", self.generation_id.as_ref());
        push_optional(&mut entries, "span_id", self.span_id.as_ref());
        entries
    }
}

/// 中文：若可选强类型字段存在，则格式化并追加到扁平输出列表。
fn push_optional<T: fmt::Display>(
    entries: &mut Vec<(&'static str, String)>,
    key: &'static str,
    value: Option<&T>,
) {
    if let Some(value) = value {
        entries.push((key, value.to_string()));
    }
}

/// 中文：若可选字符串字段存在，则复制并追加到扁平输出列表。
fn push_optional_str(
    entries: &mut Vec<(&'static str, String)>,
    key: &'static str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        entries.push((key, value.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::{EventPayload, Topic};

    #[test]
    /// 中文：验证事件元数据和定向 Agent 会映射到统一追踪字段。
    fn trace_fields_extract_core_event_metadata() {
        let event = Event::new(
            EventId::parse("evt-1").unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::empty(),
        )
        .with_request_id(RequestId::parse("req-1").unwrap())
        .with_target(EventTarget::Agent(AgentId::parse("root-agent").unwrap()));

        let fields = TraceFields::from_event(&event);

        assert_eq!(fields.event_id.unwrap().as_str(), "evt-1");
        assert_eq!(fields.request_id.unwrap().as_str(), "req-1");
        assert_eq!(fields.topic.unwrap().as_str(), "/input/user");
        assert_eq!(fields.agent_id.unwrap().as_str(), "root-agent");
    }

    #[test]
    /// 中文：验证扁平输出只包含已设置字段且顺序稳定。
    fn entries_include_only_present_values() {
        let fields = TraceFields::default()
            .with_request_id(RequestId::parse("req-1").unwrap())
            .with_provider("codex-cli")
            .with_span_id(SpanId::parse("span-1").unwrap());

        assert_eq!(
            fields.entries(),
            vec![
                ("request_id", "req-1".to_owned()),
                ("provider", "codex-cli".to_owned()),
                ("span_id", "span-1".to_owned())
            ]
        );
    }

    #[test]
    /// 中文：验证子 Span 替换局部标识但保留请求连续性。
    fn child_span_preserves_trace_continuity() {
        let parent = TraceFields::default()
            .with_request_id(RequestId::parse("req-1").unwrap())
            .with_span_id(SpanId::parse("cli.memory").unwrap());

        let child = parent.child_span(SpanId::parse("runtime.memory").unwrap());

        assert_eq!(child.request_id.unwrap().as_str(), "req-1");
        assert_eq!(child.span_id.unwrap().as_str(), "runtime.memory");
        assert_eq!(parent.continuity_key().as_deref(), Some("request_id:req-1"));
    }
}
