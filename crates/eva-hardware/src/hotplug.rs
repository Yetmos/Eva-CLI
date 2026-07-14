//! 将平台发现结果归一化为确定的设备健康状态迁移。
//!
//! 状态机只接受合法动作并返回前后状态与原因，事件发布由生命周期层完成；这样失败不会在
//! 状态机内部隐式产生外部副作用，也不会携带原始硬件句柄。
//! Hardware hotplug state machine.

use crate::state::{DeviceHealth, DeviceId};
use eva_core::{EvaError, Topic};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "hardware hotplug state machine";

/// 定义 `HotplugAction` 可取的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotplugAction {
    /// 表示 `Insert` 枚举分支。
    Insert,
    /// 表示 `Remove` 枚举分支。
    Remove,
    /// 表示 `Reconnect` 枚举分支。
    Reconnect,
    /// 表示 `Fail` 枚举分支。
    Fail,
}

/// 表示 `HotplugEvent` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotplugEvent {
    /// 记录 `device_id` 字段对应的值。
    pub device_id: DeviceId,
    /// 记录 `action` 字段对应的值。
    pub action: HotplugAction,
    /// 记录 `previous` 字段对应的值。
    pub previous: DeviceHealth,
    /// 记录 `next` 字段对应的值。
    pub next: DeviceHealth,
    /// 记录 `topic` 字段对应的值。
    pub topic: Topic,
    /// 记录 `reason` 字段对应的值。
    pub reason: String,
}

/// 表示 `HotplugStateMachine` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotplugStateMachine {
    /// 记录 `device_id` 字段对应的值。
    pub device_id: DeviceId,
    /// 记录 `health` 字段对应的值。
    pub health: DeviceHealth,
}

impl HotplugAction {
    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::Remove => "remove",
            Self::Reconnect => "reconnect",
            Self::Fail => "fail",
        }
    }
}

impl HotplugStateMachine {
    /// 创建并初始化当前类型的实例。
    pub fn new(device_id: DeviceId) -> Self {
        Self {
            device_id,
            health: DeviceHealth::Disconnected,
        }
    }

    /// 执行 `apply` 对应的处理逻辑。
    pub fn apply(
        &mut self,
        action: HotplugAction,
        reason: impl Into<String>,
    ) -> Result<HotplugEvent, EvaError> {
        let previous = self.health;
        let next = match action {
            HotplugAction::Insert => DeviceHealth::Available,
            HotplugAction::Remove => DeviceHealth::Disconnected,
            HotplugAction::Reconnect => {
                if previous != DeviceHealth::Disconnected && previous != DeviceHealth::Failed {
                    return Err(EvaError::conflict(
                        "reconnect requires disconnected or failed state",
                    )
                    .with_context("device_id", self.device_id.as_str())
                    .with_context("health", previous.as_str()));
                }
                DeviceHealth::Available
            }
            HotplugAction::Fail => DeviceHealth::Failed,
        };
        self.health = next;
        Ok(HotplugEvent {
            device_id: self.device_id.clone(),
            action,
            previous,
            next,
            topic: hotplug_topic(action)?,
            reason: reason.into(),
        })
    }
}

/// 执行 `hotplug_topic` 对应的处理逻辑。
fn hotplug_topic(action: HotplugAction) -> Result<Topic, EvaError> {
    let value = match action {
        HotplugAction::Insert | HotplugAction::Reconnect => "/hardware/connected",
        HotplugAction::Remove => "/hardware/disconnected",
        HotplugAction::Fail => "/hardware/failed",
    };
    Topic::parse(value)
}

/// 声明 `tests` 子模块。
#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 `hotplug_maps_actions_to_topics_and_health` 场景下的预期行为。
    #[test]
    fn hotplug_maps_actions_to_topics_and_health() {
        let mut machine =
            HotplugStateMachine::new(DeviceId::parse("scale-main:main-scale").unwrap());

        let inserted = machine
            .apply(HotplugAction::Insert, "manifest observed")
            .unwrap();
        assert_eq!(inserted.topic.as_str(), "/hardware/connected");
        assert_eq!(inserted.next, DeviceHealth::Available);

        let removed = machine
            .apply(HotplugAction::Remove, "device removed")
            .unwrap();
        assert_eq!(removed.topic.as_str(), "/hardware/disconnected");
        assert_eq!(removed.next, DeviceHealth::Disconnected);
    }

    /// 验证 `reconnect_requires_non_available_state` 场景下的预期行为。
    #[test]
    fn reconnect_requires_non_available_state() {
        let mut machine =
            HotplugStateMachine::new(DeviceId::parse("scale-main:main-scale").unwrap());
        machine.apply(HotplugAction::Insert, "insert").unwrap();

        let error = machine
            .apply(HotplugAction::Reconnect, "already available")
            .unwrap_err();

        assert_eq!(error.kind(), eva_core::ErrorKind::Conflict);
    }
}
