//! 中文：Agent 的有界邮箱投递实现。
//! Bounded mailbox delivery for Agents.

use eva_core::{EvaError, Event};
use std::collections::VecDeque;

/// 中文：本模块负责提供容量确定、溢出行为明确的 Agent 邮箱。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "bounded Agent mailbox delivery";

/// 中文：先进先出的有界邮箱；容量耗尽时返回错误，不静默丢弃旧事件。
/// FIFO mailbox with deterministic overflow behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMailbox {
    /// 中文：允许同时排队的最大事件数，创建后保持不变。
    capacity: usize,
    /// 中文：按到达顺序保存尚未消费的事件。
    events: VecDeque<Event>,
}

impl AgentMailbox {
    /// 中文：创建非零容量邮箱；零容量无法形成有效的投递边界，因此直接拒绝。
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

    /// 中文：把事件追加到队尾；邮箱已满时保持原队列不变并返回可用性错误。
    pub fn push(&mut self, event: Event) -> Result<(), EvaError> {
        if self.events.len() >= self.capacity {
            return Err(EvaError::unavailable("agent mailbox is full")
                .with_context("capacity", self.capacity.to_string()));
        }
        self.events.push_back(event);
        Ok(())
    }

    /// 中文：取出最早进入邮箱的事件；空邮箱返回 `None`。
    pub fn pop(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// 中文：返回当前待处理事件数。
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// 中文：判断邮箱是否没有待处理事件。
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// 中文：返回邮箱的固定容量上限。
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::{EventId, EventPayload, Topic};

    /// 中文：构造邮箱测试使用的输入事件。
    fn event(id: &str) -> Event {
        Event::new(
            EventId::parse(id).unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::empty(),
        )
    }

    #[test]
    /// 中文：验证邮箱按 FIFO 出队，并在满载时拒绝额外事件。
    fn mailbox_is_bounded_fifo() {
        let mut mailbox = AgentMailbox::new(1).unwrap();

        mailbox.push(event("evt-1")).unwrap();
        let error = mailbox.push(event("evt-2")).unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Unavailable);
        assert_eq!(mailbox.pop().unwrap().event_id().as_str(), "evt-1");
    }
}
