//! 中文：Agent 私有队列及其确定性的容量溢出行为。
//! Private Agent queue and overflow behavior.

use eva_core::{EvaError, Event};
use std::collections::VecDeque;

/// 中文：本模块负责隔离单个 Agent 的待处理事件并提供明确背压。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "private Agent queue and overflow behavior";

/// 中文：由单个 AgentRuntime 独占的有界先进先出队列。
/// Bounded FIFO queue owned by one AgentRuntime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentQueue {
    /// 中文：最多可排队的事件数量，构造后保持不变。
    capacity: usize,
    /// 中文：按接收顺序保存的待处理事件。
    events: VecDeque<Event>,
}

impl AgentQueue {
    /// 中文：创建指定容量的队列；零容量无法接收事件，因此视为无效参数。
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

    /// 中文：把事件加入队尾；满载时返回可用性错误且不修改现有事件。
    pub fn enqueue(&mut self, event: Event) -> Result<(), EvaError> {
        if self.events.len() >= self.capacity {
            return Err(EvaError::unavailable("agent queue is full")
                .with_context("capacity", self.capacity.to_string()));
        }
        self.events.push_back(event);
        Ok(())
    }

    /// 中文：取出最早入队的事件，空队列返回 `None`。
    pub fn dequeue(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// 中文：返回当前待处理事件数。
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// 中文：判断队列是否没有待处理事件。
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::{EventId, EventPayload, Topic};

    /// 中文：构造队列测试使用的事件。
    fn event(id: &str) -> Event {
        Event::new(
            EventId::parse(id).unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::empty(),
        )
    }

    #[test]
    /// 中文：验证满载队列拒绝新事件且仍按 FIFO 顺序出队。
    fn queue_is_bounded_fifo() {
        let mut queue = AgentQueue::new(1).unwrap();

        queue.enqueue(event("evt-1")).unwrap();
        assert!(queue.enqueue(event("evt-2")).is_err());

        assert_eq!(queue.dequeue().unwrap().event_id().as_str(), "evt-1");
    }
}
