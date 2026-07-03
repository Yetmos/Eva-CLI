//! Bounded mailbox delivery for Agents.

use eva_core::{EvaError, Event};
use std::collections::VecDeque;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "bounded Agent mailbox delivery";

/// FIFO mailbox with deterministic overflow behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMailbox {
    capacity: usize,
    events: VecDeque<Event>,
}

impl AgentMailbox {
    pub fn new(capacity: usize) -> Result<Self, EvaError> {
        if capacity == 0 {
            return Err(EvaError::invalid_argument(
                "mailbox capacity must be greater than zero",
            ));
        }
        Ok(Self {
            capacity,
            events: VecDeque::new(),
        })
    }

    pub fn push(&mut self, event: Event) -> Result<(), EvaError> {
        if self.events.len() >= self.capacity {
            return Err(EvaError::unavailable("agent mailbox is full")
                .with_context("capacity", self.capacity.to_string()));
        }
        self.events.push_back(event);
        Ok(())
    }

    pub fn pop(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::{EventId, EventPayload, Topic};

    fn event(id: &str) -> Event {
        Event::new(
            EventId::parse(id).unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::empty(),
        )
    }

    #[test]
    fn mailbox_is_bounded_fifo() {
        let mut mailbox = AgentMailbox::new(1).unwrap();

        mailbox.push(event("evt-1")).unwrap();
        let error = mailbox.push(event("evt-2")).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Unavailable);
        assert_eq!(mailbox.pop().unwrap().event_id().as_str(), "evt-1");
    }
}
