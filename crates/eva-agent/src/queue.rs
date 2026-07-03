//! Private Agent queue and overflow behavior.

use eva_core::{EvaError, Event};
use std::collections::VecDeque;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "private Agent queue and overflow behavior";

/// Bounded FIFO queue owned by one AgentRuntime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentQueue {
    capacity: usize,
    events: VecDeque<Event>,
}

impl AgentQueue {
    pub fn new(capacity: usize) -> Result<Self, EvaError> {
        if capacity == 0 {
            return Err(EvaError::invalid_argument(
                "agent queue capacity must be greater than zero",
            ));
        }
        Ok(Self {
            capacity,
            events: VecDeque::new(),
        })
    }

    pub fn enqueue(&mut self, event: Event) -> Result<(), EvaError> {
        if self.events.len() >= self.capacity {
            return Err(EvaError::unavailable("agent queue is full")
                .with_context("capacity", self.capacity.to_string()));
        }
        self.events.push_back(event);
        Ok(())
    }

    pub fn dequeue(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
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
    fn queue_is_bounded_fifo() {
        let mut queue = AgentQueue::new(1).unwrap();

        queue.enqueue(event("evt-1")).unwrap();
        assert!(queue.enqueue(event("evt-2")).is_err());

        assert_eq!(queue.dequeue().unwrap().event_id().as_str(), "evt-1");
    }
}
