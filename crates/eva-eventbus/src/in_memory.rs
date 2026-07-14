//! 中文：由 `eva-storage` 内存事件日志支撑的进程内 EventBus 实现。
//! In-memory EventBus implementation backed by `eva-storage` event log.

use crate::bus::{EventBus, EventReceipt};
use crate::dead_letter::{DeadLetterQueue, DeadLetterRecord};
use eva_core::{AgentId, EvaError, Event, EventId};
use eva_storage::{EventLog, EventLogRecord, InMemoryEventLog};

/// 中文：本模块为运行时提供可恢复、但不跨进程持久化的事件总线边界。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "recoverable in-process EventBus implementation boundary";

/// 中文：进程内可恢复事件总线，同时拥有日志、死信队列和发布回执历史。
/// Recoverable in-process bus used by V0.4.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryEventBus {
    /// 中文：记录发布、确认和失败状态的内存事件日志。
    log: InMemoryEventLog,
    /// 中文：保存无法完成路由或处理的事件。
    dead_letters: DeadLetterQueue,
    /// 中文：按发布顺序保存本实例产生的回执。
    receipts: Vec<EventReceipt>,
}

impl InMemoryEventBus {
    /// 中文：创建使用空日志和空死信队列的总线。
    pub fn new() -> Self {
        Self::default()
    }

    /// 中文：使用已有事件日志创建总线，供恢复或测试注入历史状态。
    pub fn with_log(log: InMemoryEventLog) -> Self {
        Self {
            log,
            dead_letters: DeadLetterQueue::new(),
            receipts: Vec::new(),
        }
    }

    /// 中文：返回底层事件日志的只读视图。
    pub fn log(&self) -> &InMemoryEventLog {
        &self.log
    }

    /// 中文：返回当前实例发布成功后保存的回执。
    pub fn receipts(&self) -> &[EventReceipt] {
        &self.receipts
    }

    /// 中文：返回死信记录的只读视图。
    pub fn dead_letters(&self) -> &[DeadLetterRecord] {
        self.dead_letters.records()
    }

    /// 中文：把无法处理的事件连同原因写入内存死信队列。
    pub fn dead_letter(&mut self, event: Event, reason: EvaError) -> DeadLetterRecord {
        self.dead_letters.push(event, reason)
    }

    /// 中文：为所有死信生成唯一子事件并重新走标准发布路径。
    ///
    /// 先完成重放事件生成，再逐个追加日志；若中途发布失败，前序事件仍然已经发布，
    /// 调用方应依赖唯一重放标识和日志事实进行恢复。
    pub fn replay_dead_letters(&mut self) -> Result<Vec<EventReceipt>, EvaError> {
        let events = self.dead_letters.replay_all_for_publish()?;
        events
            .into_iter()
            .map(|event| self.publish(event))
            .collect()
    }
}

impl EventBus for InMemoryEventBus {
    /// 中文：先把事件追加到底层日志，再从该记录构造并保存回执。
    fn publish(&mut self, event: Event) -> Result<EventReceipt, EvaError> {
        let record = self.log.append(event)?;
        let receipt = EventReceipt::from_record(&record);
        self.receipts.push(receipt.clone());
        Ok(receipt)
    }

    /// 中文：把消费者确认委托给底层日志状态机。
    fn ack(&mut self, event_id: &EventId, consumer: AgentId) -> Result<EventLogRecord, EvaError> {
        self.log.ack(event_id, consumer)
    }

    /// 中文：把消费者失败及结构化错误委托给底层日志状态机。
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

    /// 中文：构造进程内总线测试使用的文本事件。
    fn event(id: &str) -> Event {
        Event::new(
            EventId::parse(id).unwrap(),
            Topic::parse("/input/user").unwrap(),
            EventPayload::text("hello"),
        )
    }

    #[test]
    /// 中文：验证发布先写日志并返回对应序号的回执。
    fn publish_appends_to_log_and_returns_receipt() {
        let mut bus = InMemoryEventBus::new();

        let receipt = bus.publish(event("evt-1")).unwrap();

        assert_eq!(receipt.sequence, 1);
        assert_eq!(bus.log().records().len(), 1);
        assert_eq!(bus.receipts()[0].topic.as_str(), "/input/user");
    }

    #[test]
    /// 中文：验证消费者确认会更新底层日志记录。
    fn ack_updates_log_record() {
        let mut bus = InMemoryEventBus::new();
        let receipt = bus.publish(event("evt-1")).unwrap();

        let record = bus
            .ack(&receipt.event_id, AgentId::parse("root-agent").unwrap())
            .unwrap();

        assert_eq!(record.consumer.unwrap().as_str(), "root-agent");
    }

    #[test]
    /// 中文：验证无法路由的事件可以进入死信队列。
    fn dead_letter_queue_is_available() {
        let mut bus = InMemoryEventBus::new();
        let event = event("evt-1");

        bus.dead_letter(event, EvaError::not_found("no route"));

        assert_eq!(bus.dead_letters().len(), 1);
    }

    #[test]
    /// 中文：验证死信以唯一重放标识重新发布并追加到日志。
    fn dead_letters_can_be_replayed_to_log() {
        let mut bus = InMemoryEventBus::new();
        let original = event("evt-1");
        bus.publish(original.clone()).unwrap();
        bus.dead_letter(original, EvaError::not_found("no route"));

        let receipts = bus.replay_dead_letters().unwrap();

        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].event_id.as_str(), "evt-1:replay-1");
        assert_eq!(bus.dead_letters()[0].replay_count, 1);
        assert_eq!(bus.log().records().len(), 2);
    }
}
