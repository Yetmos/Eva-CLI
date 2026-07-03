//! Dead-letter records for events that cannot be delivered.

use eva_core::{EvaError, Event, EventId};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "dead-letter routing and retention boundaries";

/// One dead-lettered event plus the structured reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLetterRecord {
    pub event: Event,
    pub reason: EvaError,
    pub replay_count: usize,
}

/// In-memory dead-letter queue used by the V0.4 loop.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeadLetterQueue {
    records: Vec<DeadLetterRecord>,
}

impl DeadLetterRecord {
    pub fn new(event: Event, reason: EvaError) -> Self {
        Self {
            event,
            reason,
            replay_count: 0,
        }
    }

    pub fn event_id(&self) -> &EventId {
        self.event.event_id()
    }
}

impl DeadLetterQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, event: Event, reason: EvaError) -> DeadLetterRecord {
        let record = DeadLetterRecord::new(event, reason);
        self.records.push(record.clone());
        record
    }

    pub fn records(&self) -> &[DeadLetterRecord] {
        &self.records
    }

    pub fn replay_all(&mut self) -> Vec<Event> {
        self.records
            .iter_mut()
            .map(|record| {
                record.replay_count += 1;
                record.event.clone()
            })
            .collect()
    }

    pub fn replay_event(&mut self, event_id: &EventId) -> Result<Event, EvaError> {
        let record = self
            .records
            .iter_mut()
            .find(|record| record.event_id() == event_id)
            .ok_or_else(|| {
                EvaError::not_found("dead-letter event does not exist")
                    .with_context("event_id", event_id.as_str())
            })?;
        record.replay_count += 1;
        Ok(record.event.clone())
    }

    pub fn replay_all_for_publish(&mut self) -> Result<Vec<Event>, EvaError> {
        self.records
            .iter_mut()
            .map(|record| {
                record.replay_count += 1;
                let replay_id = EventId::parse(&format!(
                    "{}:replay-{}",
                    record.event.event_id().as_str(),
                    record.replay_count
                ))?;
                Ok(record
                    .event
                    .child_event(
                        replay_id,
                        record.event.topic().clone(),
                        record.event.payload().clone(),
                    )
                    .with_target(record.event.target().clone()))
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::{EventId, EventPayload, Topic};

    #[test]
    fn queue_records_reason() {
        let event = Event::new(
            EventId::parse("evt-1").unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::empty(),
        );
        let mut queue = DeadLetterQueue::new();

        queue.push(event, EvaError::not_found("missing route"));

        assert_eq!(queue.records().len(), 1);
        assert_eq!(
            queue.records()[0].reason.kind(),
            eva_core::ErrorKind::NotFound
        );
    }

    #[test]
    fn replay_marks_record_and_returns_event() {
        let event = Event::new(
            EventId::parse("evt-1").unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::empty(),
        );
        let mut queue = DeadLetterQueue::new();
        let event_id = event.event_id().clone();
        queue.push(event, EvaError::not_found("missing route"));

        let replayed = queue.replay_event(&event_id).unwrap();

        assert_eq!(replayed.event_id().as_str(), "evt-1");
        assert_eq!(queue.records()[0].replay_count, 1);
    }
}
