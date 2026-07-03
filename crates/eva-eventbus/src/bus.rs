//! EventBus contracts for publication and consumer acknowledgements.

use eva_core::{AgentId, EvaError, Event, EventId, EventTarget, Topic};
use eva_storage::EventLogRecord;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "event publication and subscription-facing bus contracts";

/// Receipt returned after an event has crossed the publish boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventReceipt {
    pub event_id: EventId,
    pub sequence: u64,
    pub topic: Topic,
    pub target: EventTarget,
}

/// Synchronous EventBus operations needed by the V0.4 runtime loop.
pub trait EventBus {
    fn publish(&mut self, event: Event) -> Result<EventReceipt, EvaError>;
    fn ack(&mut self, event_id: &EventId, consumer: AgentId) -> Result<EventLogRecord, EvaError>;
    fn fail(
        &mut self,
        event_id: &EventId,
        consumer: AgentId,
        error: EvaError,
    ) -> Result<EventLogRecord, EvaError>;
}

impl EventReceipt {
    pub fn from_record(record: &EventLogRecord) -> Self {
        Self {
            event_id: record.event.event_id().clone(),
            sequence: record.sequence,
            topic: record.event.topic().clone(),
            target: record.event.target().clone(),
        }
    }
}
