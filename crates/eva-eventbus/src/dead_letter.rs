//! Dead-letter records for events that cannot be delivered.

use eva_core::{EvaError, Event};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "dead-letter routing and retention boundaries";

/// One dead-lettered event plus the structured reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLetterRecord {
    pub event: Event,
    pub reason: EvaError,
}

/// In-memory dead-letter queue used by the V0.4 loop.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeadLetterQueue {
    records: Vec<DeadLetterRecord>,
}

impl DeadLetterRecord {
    pub fn new(event: Event, reason: EvaError) -> Self {
        Self { event, reason }
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
}
