//! 中文：EventBus 的发布、消费确认和失败记录契约。
//! EventBus contracts for publication and consumer acknowledgements.

use eva_core::{AgentId, EvaError, Event, EventId, EventTarget, Topic};
use eva_storage::EventLogRecord;

/// 中文：本模块定义事件跨越总线边界后必须提供的稳定同步操作。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "event publication and subscription-facing bus contracts";

/// 中文：事件成功跨越发布边界后返回的不可变回执。
/// Receipt returned after an event has crossed the publish boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventReceipt {
    /// 中文：已发布事件的稳定标识。
    pub event_id: EventId,
    /// 中文：底层事件日志分配的单调序号。
    pub sequence: u64,
    /// 中文：发布时的事件主题快照。
    pub topic: Topic,
    /// 中文：发布时的目标快照，用于区分广播和定向事件。
    pub target: EventTarget,
}

/// 中文：运行时循环所需的同步 EventBus 操作。
///
/// 实现必须先持久化或写入其日志边界，再返回发布回执；确认与失败操作都以事件标识
/// 和消费者标识关联同一条日志记录，调用方不能把返回成功理解为仅在内存中排队。
/// Synchronous EventBus operations needed by the V0.4 runtime loop.
pub trait EventBus {
    /// 中文：发布一个事件并返回底层日志分配的回执。
    fn publish(&mut self, event: Event) -> Result<EventReceipt, EvaError>;
    /// 中文：记录指定消费者已经成功处理事件。
    fn ack(&mut self, event_id: &EventId, consumer: AgentId) -> Result<EventLogRecord, EvaError>;
    /// 中文：记录指定消费者处理失败，并保留结构化错误供恢复决策使用。
    fn fail(
        &mut self,
        event_id: &EventId,
        consumer: AgentId,
        error: EvaError,
    ) -> Result<EventLogRecord, EvaError>;
}

impl EventReceipt {
    /// 中文：从已追加的日志记录提取发布回执，确保回执序号与持久化事实一致。
    pub fn from_record(record: &EventLogRecord) -> Self {
        Self {
            event_id: record.event.event_id().clone(),
            sequence: record.sequence,
            topic: record.event.topic().clone(),
            target: record.event.target().clone(),
        }
    }
}
