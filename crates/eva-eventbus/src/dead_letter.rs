//! 中文：无法正常投递事件的死信记录、重驱策略和内存队列。
//! Dead-letter records for events that cannot be delivered.

use eva_core::{EvaError, Event, EventId};

/// 中文：本模块负责保留失败事件、结构化原因和可重放状态。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "dead-letter routing and retention boundaries";

/// 中文：一条死信事件及其失败原因和重驱状态。
/// One dead-lettered event plus the structured reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLetterRecord {
    /// 中文：保留完整元数据和载荷的原始失败事件。
    pub event: Event,
    /// 中文：事件进入死信队列的结构化原因。
    pub reason: EvaError,
    /// 中文：已从该记录生成重放事件的累计次数。
    pub replay_count: usize,
    /// 中文：后续自动或人工重驱使用的时间策略。
    pub redrive: RedrivePolicy,
}

/// 中文：持久化死信记录使用的重驱时间元数据。
/// Redrive timing metadata for durable dead-letter records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RedrivePolicy {
    /// 中文：相邻两次重驱之间的基础延迟毫秒数。
    pub retry_delay_ms: u64,
    /// 中文：记录再次具备重驱资格的相对时间阈值。
    pub next_attempt_after_ms: u64,
}

/// 中文：运行时循环使用的内存死信队列；记录按进入顺序保存且不会自动删除。
/// In-memory dead-letter queue used by the V0.4 loop.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeadLetterQueue {
    /// 中文：当前保留的全部死信记录。
    records: Vec<DeadLetterRecord>,
}

impl DeadLetterRecord {
    /// 中文：从失败事件和原因创建尚未重放、无延迟策略的记录。
    pub fn new(event: Event, reason: EvaError) -> Self {
        Self {
            event,
            reason,
            replay_count: 0,
            redrive: RedrivePolicy::default(),
        }
    }

    /// 中文：返回原始事件标识，供查询和定向重驱使用。
    pub fn event_id(&self) -> &EventId {
        self.event.event_id()
    }
}

impl DeadLetterQueue {
    /// 中文：创建空死信队列。
    pub fn new() -> Self {
        Self::default()
    }

    /// 中文：把失败事件追加到队尾，并返回与队列内状态一致的记录快照。
    pub fn push(&mut self, event: Event, reason: EvaError) -> DeadLetterRecord {
        let record = DeadLetterRecord::new(event, reason);
        self.records.push(record.clone());
        record
    }

    /// 中文：返回所有死信记录的只读切片。
    pub fn records(&self) -> &[DeadLetterRecord] {
        &self.records
    }

    /// 中文：克隆所有原始事件用于诊断性重放，并递增每条记录的重放计数。
    ///
    /// 此方法保留原事件标识，适合不经过发布边界的调用；需要再次发布时应使用
    /// `replay_all_for_publish` 生成唯一子事件标识。
    pub fn replay_all(&mut self) -> Vec<Event> {
        self.records
            .iter_mut()
            .map(|record| {
                record.replay_count += 1;
                record.event.clone()
            })
            .collect()
    }

    /// 中文：按标识克隆一条原始死信事件并递增计数；记录不存在时返回明确错误。
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

    /// 中文：为全部死信生成可重新发布的子事件，并保留原事件的因果链和目标。
    ///
    /// 新标识包含递增的重放序号，避免事件日志把重驱误判为重复发布。
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

    /// 中文：判断队列是否没有死信记录。
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eva_core::{EventId, EventPayload, Topic};

    #[test]
    /// 中文：验证入队会保留结构化失败原因。
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
    /// 中文：验证定向重放返回原事件并递增记录计数。
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
