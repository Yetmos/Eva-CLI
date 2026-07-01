//! Event data contracts shared by the EventBus, scheduler, and runtimes.

use crate::capability::CapabilityName;
use crate::ids::{AdapterId, AgentId, EventId, GenerationId, RequestId};
use crate::topic::Topic;
use std::time::SystemTime;

/// Opaque event payload carried without schema interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum EventPayload {
    /// Explicit empty payload.
    #[default]
    Empty,
    /// UTF-8 text payload.
    Text(String),
    /// Binary payload.
    Bytes(Vec<u8>),
}

impl EventPayload {
    /// Creates an empty payload.
    pub fn empty() -> Self {
        Self::Empty
    }

    /// Creates a text payload.
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }

    /// Creates a binary payload.
    pub fn bytes(value: impl Into<Vec<u8>>) -> Self {
        Self::Bytes(value.into())
    }

    /// Returns true when the payload carries no bytes or text.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::Text(value) => value.is_empty(),
            Self::Bytes(value) => value.is_empty(),
        }
    }

    /// Returns the text payload, when this payload is text.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value.as_str()),
            Self::Empty | Self::Bytes(_) => None,
        }
    }

    /// Returns the byte payload, when this payload is binary.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(value) => Some(value.as_slice()),
            Self::Empty | Self::Text(_) => None,
        }
    }
}

impl From<String> for EventPayload {
    fn from(value: String) -> Self {
        Self::text(value)
    }
}

impl From<&str> for EventPayload {
    fn from(value: &str) -> Self {
        Self::text(value)
    }
}

impl From<Vec<u8>> for EventPayload {
    fn from(value: Vec<u8>) -> Self {
        Self::bytes(value)
    }
}

impl From<&[u8]> for EventPayload {
    fn from(value: &[u8]) -> Self {
        Self::bytes(value.to_vec())
    }
}

/// Desired event delivery target.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum EventTarget {
    /// No fixed target; scheduler may select recipients by topic.
    #[default]
    Broadcast,
    /// Direct the event to an Agent.
    Agent(AgentId),
    /// Direct the event to a Capability.
    Capability(CapabilityName),
    /// Direct the event to an Adapter.
    Adapter(AdapterId),
}

impl EventTarget {
    /// Returns true when the event has an explicit non-broadcast target.
    pub fn is_directed(&self) -> bool {
        !matches!(self, Self::Broadcast)
    }
}

/// Correlation and causation linkage for event chains.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct TraceContext {
    correlation_id: Option<EventId>,
    causation_id: Option<EventId>,
}

impl TraceContext {
    /// Creates a trace context.
    pub fn new(correlation_id: Option<EventId>, causation_id: Option<EventId>) -> Self {
        Self {
            correlation_id,
            causation_id,
        }
    }

    /// Creates a context with a correlation id.
    pub fn correlated(correlation_id: EventId) -> Self {
        Self::new(Some(correlation_id), None)
    }

    /// Returns the root correlation id, when present.
    pub fn correlation_id(&self) -> Option<&EventId> {
        self.correlation_id.as_ref()
    }

    /// Returns the direct causation id, when present.
    pub fn causation_id(&self) -> Option<&EventId> {
        self.causation_id.as_ref()
    }

    /// Returns a child trace where causation points at `parent_event_id`.
    pub fn child_of(&self, parent_event_id: EventId) -> Self {
        Self {
            correlation_id: self
                .correlation_id
                .clone()
                .or(Some(parent_event_id.clone())),
            causation_id: Some(parent_event_id),
        }
    }
}

/// Non-business event metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventMetadata {
    created_at: SystemTime,
    request_id: Option<RequestId>,
    trace: TraceContext,
    generation_id: Option<GenerationId>,
}

impl Default for EventMetadata {
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
    /// Creates metadata with the current system timestamp.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the creation timestamp.
    pub fn created_at(&self) -> SystemTime {
        self.created_at
    }

    /// Returns the request id, when present.
    pub fn request_id(&self) -> Option<&RequestId> {
        self.request_id.as_ref()
    }

    /// Returns the trace context.
    pub fn trace(&self) -> &TraceContext {
        &self.trace
    }

    /// Returns the generation id, when present.
    pub fn generation_id(&self) -> Option<&GenerationId> {
        self.generation_id.as_ref()
    }

    /// Overrides the creation timestamp.
    pub fn with_created_at(mut self, created_at: SystemTime) -> Self {
        self.created_at = created_at;
        self
    }

    /// Sets the request id.
    pub fn with_request_id(mut self, request_id: RequestId) -> Self {
        self.request_id = Some(request_id);
        self
    }

    /// Sets the trace context.
    pub fn with_trace(mut self, trace: TraceContext) -> Self {
        self.trace = trace;
        self
    }

    /// Sets the generation id.
    pub fn with_generation_id(mut self, generation_id: GenerationId) -> Self {
        self.generation_id = Some(generation_id);
        self
    }
}

/// Standard event value passed between Eva modules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    event_id: EventId,
    topic: Topic,
    target: EventTarget,
    payload: EventPayload,
    metadata: EventMetadata,
}

impl Event {
    /// Creates a broadcast event with default metadata.
    pub fn new(event_id: EventId, topic: Topic, payload: EventPayload) -> Self {
        Self {
            event_id,
            topic,
            target: EventTarget::Broadcast,
            payload,
            metadata: EventMetadata::new(),
        }
    }

    /// Returns the event id.
    pub fn event_id(&self) -> &EventId {
        &self.event_id
    }

    /// Returns the topic.
    pub fn topic(&self) -> &Topic {
        &self.topic
    }

    /// Returns the event target.
    pub fn target(&self) -> &EventTarget {
        &self.target
    }

    /// Returns the opaque payload.
    pub fn payload(&self) -> &EventPayload {
        &self.payload
    }

    /// Returns metadata.
    pub fn metadata(&self) -> &EventMetadata {
        &self.metadata
    }

    /// Sets an explicit event target.
    pub fn with_target(mut self, target: EventTarget) -> Self {
        self.target = target;
        self
    }

    /// Sets the request id on metadata.
    pub fn with_request_id(mut self, request_id: RequestId) -> Self {
        self.metadata = self.metadata.with_request_id(request_id);
        self
    }

    /// Sets trace context on metadata.
    pub fn with_trace(mut self, trace: TraceContext) -> Self {
        self.metadata = self.metadata.with_trace(trace);
        self
    }

    /// Sets generation id on metadata.
    pub fn with_generation_id(mut self, generation_id: GenerationId) -> Self {
        self.metadata = self.metadata.with_generation_id(generation_id);
        self
    }

    /// Replaces metadata.
    pub fn with_metadata(mut self, metadata: EventMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Creates a child event whose trace points back to this event.
    pub fn child_event(&self, event_id: EventId, topic: Topic, payload: EventPayload) -> Self {
        let trace = self.metadata.trace().child_of(self.event_id.clone());
        Self::new(event_id, topic, payload).with_trace(trace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_id(value: &str) -> EventId {
        EventId::parse(value).unwrap()
    }

    fn topic(value: &str) -> Topic {
        Topic::parse(value).unwrap()
    }

    #[test]
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
    fn event_accepts_broadcast_target() {
        let event = Event::new(event_id("evt-1"), topic("/a/b"), EventPayload::empty());
        assert_eq!(event.target(), &EventTarget::Broadcast);
        assert!(!event.target().is_directed());
    }

    #[test]
    fn event_accepts_agent_target() {
        let agent_id = AgentId::parse("agent-root").unwrap();
        let event = Event::new(event_id("evt-1"), topic("/a/b"), EventPayload::empty())
            .with_target(EventTarget::Agent(agent_id.clone()));

        assert_eq!(event.target(), &EventTarget::Agent(agent_id));
        assert!(event.target().is_directed());
    }

    #[test]
    fn event_accepts_capability_and_adapter_targets() {
        let capability = CapabilityName::parse("repo.summary").unwrap();
        let adapter = AdapterId::parse("adapter-cli").unwrap();

        assert!(EventTarget::Capability(capability).is_directed());
        assert!(EventTarget::Adapter(adapter).is_directed());
    }

    #[test]
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
    fn payload_can_be_text_or_bytes() {
        assert_eq!(EventPayload::from("hello").as_text(), Some("hello"));
        assert_eq!(EventPayload::from(vec![1, 2]).as_bytes(), Some(&[1, 2][..]));
    }
}
