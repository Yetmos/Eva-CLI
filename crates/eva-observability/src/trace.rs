//! Trace fields and propagation contracts.

use eva_core::{
    AdapterId, AgentId, CapabilityName, Event, EventId, EventTarget, GenerationId, RequestId, Topic,
};
use std::fmt;

/// Stable span identifier carried by observability records.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpanId(String);

/// Common trace fields that all runtime, adapter, and CLI records can share.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TraceFields {
    pub event_id: Option<EventId>,
    pub request_id: Option<RequestId>,
    pub topic: Option<Topic>,
    pub agent_id: Option<AgentId>,
    pub adapter_id: Option<AdapterId>,
    pub capability: Option<CapabilityName>,
    pub provider: Option<String>,
    pub correlation_id: Option<EventId>,
    pub causation_id: Option<EventId>,
    pub generation_id: Option<GenerationId>,
    pub span_id: Option<SpanId>,
}

impl SpanId {
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

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SpanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TraceFields {
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

    pub fn with_agent_id(mut self, value: AgentId) -> Self {
        self.agent_id = Some(value);
        self
    }

    pub fn with_request_id(mut self, value: RequestId) -> Self {
        self.request_id = Some(value);
        self
    }

    pub fn with_adapter_id(mut self, value: AdapterId) -> Self {
        self.adapter_id = Some(value);
        self
    }

    pub fn with_capability(mut self, value: CapabilityName) -> Self {
        self.capability = Some(value);
        self
    }

    pub fn with_provider(mut self, value: impl Into<String>) -> Self {
        self.provider = Some(value.into());
        self
    }

    pub fn with_span_id(mut self, value: SpanId) -> Self {
        self.span_id = Some(value);
        self
    }

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

fn push_optional<T: fmt::Display>(
    entries: &mut Vec<(&'static str, String)>,
    key: &'static str,
    value: Option<&T>,
) {
    if let Some(value) = value {
        entries.push((key, value.to_string()));
    }
}

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
}
