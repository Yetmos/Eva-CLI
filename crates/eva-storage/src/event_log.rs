//! Durable event log placeholders.

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "durable event log interfaces and replay boundaries";

use eva_core::{AgentId, EvaError, Event, EventId};

/// Lifecycle state for one event log record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventLogStatus {
    Appended,
    Acked,
    Failed,
}

/// Append-only event log record used by EventBus and Agent consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventLogRecord {
    pub sequence: u64,
    pub event: Event,
    pub status: EventLogStatus,
    pub consumer: Option<AgentId>,
    pub error: Option<EvaError>,
}

/// Event log behavior required by the V0.4 runtime loop.
pub trait EventLog {
    fn append(&mut self, event: Event) -> Result<EventLogRecord, EvaError>;
    fn ack(&mut self, event_id: &EventId, consumer: AgentId) -> Result<EventLogRecord, EvaError>;
    fn fail(
        &mut self,
        event_id: &EventId,
        consumer: AgentId,
        error: EvaError,
    ) -> Result<EventLogRecord, EvaError>;
    fn replay_from(&self, sequence: u64) -> Vec<EventLogRecord>;
    fn watermark(&self) -> u64;
}

/// In-memory log used by tests and the V0.4 basic runtime path.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryEventLog {
    next_sequence: u64,
    records: Vec<EventLogRecord>,
}

impl EventLogStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Appended => "appended",
            Self::Acked => "acked",
            Self::Failed => "failed",
        }
    }
}

impl EventLogRecord {
    fn appended(sequence: u64, event: Event) -> Self {
        Self {
            sequence,
            event,
            status: EventLogStatus::Appended,
            consumer: None,
            error: None,
        }
    }
}

impl InMemoryEventLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn records(&self) -> &[EventLogRecord] {
        &self.records
    }

    fn find_mut(&mut self, event_id: &EventId) -> Result<&mut EventLogRecord, EvaError> {
        self.records
            .iter_mut()
            .find(|record| record.event.event_id() == event_id)
            .ok_or_else(|| {
                EvaError::not_found("event log record does not exist")
                    .with_context("event_id", event_id.as_str())
            })
    }

    fn contains_event(&self, event_id: &EventId) -> bool {
        self.records
            .iter()
            .any(|record| record.event.event_id() == event_id)
    }
}

impl EventLog for InMemoryEventLog {
    fn append(&mut self, event: Event) -> Result<EventLogRecord, EvaError> {
        if self.contains_event(event.event_id()) {
            return Err(EvaError::conflict("event already exists in log")
                .with_context("event_id", event.event_id().as_str()));
        }

        self.next_sequence += 1;
        let record = EventLogRecord::appended(self.next_sequence, event);
        self.records.push(record.clone());
        Ok(record)
    }

    fn ack(&mut self, event_id: &EventId, consumer: AgentId) -> Result<EventLogRecord, EvaError> {
        let record = self.find_mut(event_id)?;
        record.status = EventLogStatus::Acked;
        record.consumer = Some(consumer);
        record.error = None;
        Ok(record.clone())
    }

    fn fail(
        &mut self,
        event_id: &EventId,
        consumer: AgentId,
        error: EvaError,
    ) -> Result<EventLogRecord, EvaError> {
        let record = self.find_mut(event_id)?;
        record.status = EventLogStatus::Failed;
        record.consumer = Some(consumer);
        record.error = Some(error);
        Ok(record.clone())
    }

    fn replay_from(&self, sequence: u64) -> Vec<EventLogRecord> {
        self.records
            .iter()
            .filter(|record| record.sequence >= sequence)
            .cloned()
            .collect()
    }

    fn watermark(&self) -> u64 {
        self.next_sequence
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::{EventPayload, Topic};

    fn event(id: &str) -> Event {
        Event::new(
            EventId::parse(id).unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::text("hello"),
        )
    }

    #[test]
    fn append_assigns_sequence_and_watermark() {
        let mut log = InMemoryEventLog::new();

        let first = log.append(event("evt-1")).unwrap();
        let second = log.append(event("evt-2")).unwrap();

        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert_eq!(log.watermark(), 2);
    }

    #[test]
    fn ack_marks_consumer() {
        let mut log = InMemoryEventLog::new();
        let event = event("evt-1");
        let event_id = event.event_id().clone();
        log.append(event).unwrap();

        let record = log
            .ack(&event_id, AgentId::parse("root-agent").unwrap())
            .unwrap();

        assert_eq!(record.status, EventLogStatus::Acked);
        assert_eq!(record.consumer.unwrap().as_str(), "root-agent");
    }

    #[test]
    fn fail_preserves_structured_error() {
        let mut log = InMemoryEventLog::new();
        let event = event("evt-1");
        let event_id = event.event_id().clone();
        log.append(event).unwrap();

        let record = log
            .fail(
                &event_id,
                AgentId::parse("root-agent").unwrap(),
                EvaError::unavailable("handler offline"),
            )
            .unwrap();

        assert_eq!(record.status, EventLogStatus::Failed);
        assert!(record.error.unwrap().is_retryable());
    }

    #[test]
    fn replay_returns_records_from_cursor() {
        let mut log = InMemoryEventLog::new();
        log.append(event("evt-1")).unwrap();
        log.append(event("evt-2")).unwrap();

        let replay = log.replay_from(2);

        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].event.event_id().as_str(), "evt-2");
    }
}
