//! Hardware hotplug state machine.

use crate::state::{DeviceHealth, DeviceId};
use eva_core::{EvaError, Topic};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "hardware hotplug state machine";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotplugAction {
    Insert,
    Remove,
    Reconnect,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotplugEvent {
    pub device_id: DeviceId,
    pub action: HotplugAction,
    pub previous: DeviceHealth,
    pub next: DeviceHealth,
    pub topic: Topic,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotplugStateMachine {
    pub device_id: DeviceId,
    pub health: DeviceHealth,
}

impl HotplugAction {
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
    pub fn new(device_id: DeviceId) -> Self {
        Self {
            device_id,
            health: DeviceHealth::Disconnected,
        }
    }

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

fn hotplug_topic(action: HotplugAction) -> Result<Topic, EvaError> {
    let value = match action {
        HotplugAction::Insert | HotplugAction::Reconnect => "/hardware/connected",
        HotplugAction::Remove => "/hardware/disconnected",
        HotplugAction::Fail => "/hardware/failed",
    };
    Topic::parse(value)
}

#[cfg(test)]
mod tests {
    use super::*;

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
