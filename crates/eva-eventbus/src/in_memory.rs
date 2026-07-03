//! In-memory EventBus implementation backed by `eva-storage` event log.

use crate::bus::{EventBus, EventReceipt};
use crate::dead_letter::{DeadLetterQueue, DeadLetterRecord};
use eva_core::{AgentId, EvaError, Event, EventId};
use eva_storage::{EventLog, EventLogRecord, InMemoryEventLog};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "recoverable in-process EventBus implementation boundary";

/// Recoverable in-process bus used by V0.4.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryEventBus {
    log: InMemoryEventLog,
    dead_letters: DeadLetterQueue,
    receipts: Vec<EventReceipt>,
}

impl InMemoryEventBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_log(log: InMemoryEventLog) -> Self {
        Self {
            log,
            dead_letters: DeadLetterQueue::new(),
            receipts: Vec::new(),
        }
    }

    pub fn log(&self) -> &InMemoryEventLog {
        &self.log
    }

    pub fn receipts(&self) -> &[EventReceipt] {
        &self.receipts
    }

    pub fn dead_letters(&self) -> &[DeadLetterRecord] {
        self.dead_letters.records()
    }

    pub fn dead_letter(&mut self, event: Event, reason: EvaError) -> DeadLetterRecord {
        self.dead_letters.push(event, reason)
    }
}

impl EventBus for InMemoryEventBus {
    fn publish(&mut self, event: Event) -> Result<EventReceipt, EvaError> {
        let record = self.log.append(event)?;
        let receipt = EventReceipt::from_record(&record);
        self.receipts.push(receipt.clone());
        Ok(receipt)
    }

    fn ack(&mut self, event_id: &EventId, consumer: AgentId) -> Result<EventLogRecord, EvaError> {
        self.log.ack(event_id, consumer)
    }

    fn fail(
        &mut self,
        event_id: &EventId,
        consumer: AgentId,
        error: EvaError,
    ) -> Result<EventLogRecord, EvaError> {
        self.log.fail(event_id, consumer, error)
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
    fn publish_appends_to_log_and_returns_receipt() {
        let mut bus = InMemoryEventBus::new();

        let receipt = bus.publish(event("evt-1")).unwrap();

        assert_eq!(receipt.sequence, 1);
        assert_eq!(bus.log().records().len(), 1);
        assert_eq!(bus.receipts()[0].topic.as_str(), "/input/user");
    }

    #[test]
    fn ack_updates_log_record() {
        let mut bus = InMemoryEventBus::new();
        let receipt = bus.publish(event("evt-1")).unwrap();

        let record = bus
            .ack(&receipt.event_id, AgentId::parse("root-agent").unwrap())
            .unwrap();

        assert_eq!(record.consumer.unwrap().as_str(), "root-agent");
    }

    #[test]
    fn dead_letter_queue_is_available() {
        let mut bus = InMemoryEventBus::new();
        let event = event("evt-1");

        bus.dead_letter(event, EvaError::not_found("no route"));

        assert_eq!(bus.dead_letters().len(), 1);
    }
}
