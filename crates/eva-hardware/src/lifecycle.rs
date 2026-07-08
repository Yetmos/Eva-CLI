//! Hardware driver lifecycle coordination.

use crate::registry::{DeviceLease, DeviceRegistry, RegisteredDevice};
use crate::state::DeviceId;
use crate::{HotplugAction, HotplugEvent, HotplugStateMachine};
use eva_core::{CapabilityName, EvaError, Event, EventId, EventPayload, RequestId, TraceContext};
use eva_eventbus::{EventBus, EventReceipt};
use eva_observability::{AuditAction, AuditEvent, AuditOutcome, AuditSink, TraceFields};
use eva_policy::{HighRiskAction, RuntimePolicyGate, RuntimePolicyRequest};
use std::collections::BTreeMap;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "hardware driver lifecycle with OS permission, policy, lease, hotplug, and audit gates";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsPermissionCheck {
    pub device_id: DeviceId,
    pub bus: String,
    pub permission: String,
    pub granted: bool,
}

pub trait OsPermissionProvider {
    fn check(&self, device: &RegisteredDevice) -> Result<OsPermissionCheck, EvaError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticOsPermissionProvider {
    permission: String,
    granted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverStartRequest {
    pub device_id: DeviceId,
    pub request_id: RequestId,
    pub capability: CapabilityName,
    pub driver_id: String,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverLifecycleState {
    Opened,
    Stopped,
    Crashed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverLifecycleReport {
    pub device_id: DeviceId,
    pub request_id: RequestId,
    pub driver_id: String,
    pub state: DriverLifecycleState,
    pub audit: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveDriverSession {
    pub lease: DeviceLease,
    pub driver_id: String,
    pub capability: CapabilityName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotplugPublishReport {
    pub event: HotplugEvent,
    pub receipt: EventReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareLifecycleCoordinator<P> {
    registry: DeviceRegistry,
    permission_provider: P,
    active: BTreeMap<DeviceId, ActiveDriverSession>,
}

impl StaticOsPermissionProvider {
    pub fn granted(permission: impl Into<String>) -> Self {
        Self {
            permission: permission.into(),
            granted: true,
        }
    }

    pub fn denied(permission: impl Into<String>) -> Self {
        Self {
            permission: permission.into(),
            granted: false,
        }
    }
}

impl OsPermissionProvider for StaticOsPermissionProvider {
    fn check(&self, device: &RegisteredDevice) -> Result<OsPermissionCheck, EvaError> {
        Ok(OsPermissionCheck {
            device_id: device.identity.id.clone(),
            bus: device.identity.bus.as_str().to_owned(),
            permission: self.permission.clone(),
            granted: self.granted,
        })
    }
}

impl DriverStartRequest {
    pub fn new(
        device_id: DeviceId,
        request_id: RequestId,
        capability: CapabilityName,
        driver_id: impl Into<String>,
    ) -> Self {
        Self {
            device_id,
            request_id,
            capability,
            driver_id: driver_id.into(),
            timeout_ms: None,
        }
    }

    pub fn with_timeout_ms(mut self, value: u64) -> Self {
        self.timeout_ms = Some(value);
        self
    }
}

impl DriverLifecycleState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Opened => "opened",
            Self::Stopped => "stopped",
            Self::Crashed => "crashed",
        }
    }
}

impl<P> HardwareLifecycleCoordinator<P>
where
    P: OsPermissionProvider,
{
    pub fn new(registry: DeviceRegistry, permission_provider: P) -> Self {
        Self {
            registry,
            permission_provider,
            active: BTreeMap::new(),
        }
    }

    pub fn registry(&self) -> &DeviceRegistry {
        &self.registry
    }

    pub fn active_session(&self, device_id: &DeviceId) -> Option<&ActiveDriverSession> {
        self.active.get(device_id)
    }

    pub fn start_driver<S>(
        &mut self,
        request: DriverStartRequest,
        policy_gate: &RuntimePolicyGate,
        audit_sink: &mut S,
    ) -> Result<DriverLifecycleReport, EvaError>
    where
        S: AuditSink,
    {
        if self.active.contains_key(&request.device_id) {
            return Err(EvaError::conflict("hardware driver is already active")
                .with_context("device_id", request.device_id.as_str()));
        }
        let device = self
            .registry
            .get(&request.device_id)
            .cloned()
            .ok_or_else(|| {
                EvaError::not_found("device is not registered")
                    .with_context("device_id", request.device_id.as_str())
            })?;
        let mut policy_request = RuntimePolicyRequest::new(HighRiskAction::HardwareBind)
            .with_bus(device.identity.bus.as_str())
            .with_capability(request.capability.clone());
        if let Some(timeout_ms) = request.timeout_ms {
            policy_request = policy_request.with_timeout_ms(timeout_ms);
        }
        let decision = policy_gate.decide(policy_request);
        decision.ensure_allowed()?;

        let permission = self.permission_provider.check(&device)?;
        ensure_os_permission(&permission)?;

        let lease = self
            .registry
            .claim(&request.device_id, request.request_id.clone())?;
        let report = DriverLifecycleReport {
            device_id: request.device_id.clone(),
            request_id: request.request_id.clone(),
            driver_id: request.driver_id.clone(),
            state: DriverLifecycleState::Opened,
            audit: vec![
                format!("policy:{}", decision.reason),
                format!("os_permission:{}", permission.permission),
                "lease:claimed".to_owned(),
                "driver:opened".to_owned(),
            ],
        };
        self.active.insert(
            request.device_id.clone(),
            ActiveDriverSession {
                lease,
                driver_id: request.driver_id,
                capability: request.capability,
            },
        );
        record_lifecycle_audit(
            audit_sink,
            AuditAction::HardwareDriverStarted,
            AuditOutcome::Ok,
            &report,
        )?;
        Ok(report)
    }

    pub fn stop_driver<S>(
        &mut self,
        device_id: &DeviceId,
        request_id: &RequestId,
        audit_sink: &mut S,
    ) -> Result<DriverLifecycleReport, EvaError>
    where
        S: AuditSink,
    {
        let session = self.active.get(device_id).ok_or_else(|| {
            EvaError::not_found("hardware driver is not active")
                .with_context("device_id", device_id.as_str())
        })?;
        if &session.lease.request_id != request_id {
            return Err(
                EvaError::permission_denied("device lease does not match claimant")
                    .with_context("device_id", device_id.as_str())
                    .with_context("request_id", request_id.as_str()),
            );
        }
        let session = self
            .active
            .remove(device_id)
            .expect("active session checked above");
        self.registry.release(&session.lease)?;
        let report = DriverLifecycleReport {
            device_id: device_id.clone(),
            request_id: request_id.clone(),
            driver_id: session.driver_id,
            state: DriverLifecycleState::Stopped,
            audit: vec!["driver:stopped".to_owned(), "lease:released".to_owned()],
        };
        record_lifecycle_audit(
            audit_sink,
            AuditAction::HardwareDriverStopped,
            AuditOutcome::Ok,
            &report,
        )?;
        Ok(report)
    }

    pub fn record_driver_crash<S>(
        &mut self,
        device_id: &DeviceId,
        reason: impl Into<String>,
        audit_sink: &mut S,
    ) -> Result<DriverLifecycleReport, EvaError>
    where
        S: AuditSink,
    {
        let reason = reason.into();
        let session = self.active.remove(device_id).ok_or_else(|| {
            EvaError::not_found("hardware driver is not active")
                .with_context("device_id", device_id.as_str())
        })?;
        self.registry.release(&session.lease)?;
        let report = DriverLifecycleReport {
            device_id: device_id.clone(),
            request_id: session.lease.request_id.clone(),
            driver_id: session.driver_id,
            state: DriverLifecycleState::Crashed,
            audit: vec![
                format!("driver:crashed:{reason}"),
                "lease:released".to_owned(),
            ],
        };
        record_lifecycle_audit(
            audit_sink,
            AuditAction::HardwareDriverStopped,
            AuditOutcome::Failed,
            &report,
        )?;
        Ok(report)
    }
}

pub fn publish_hotplug_event<B, S>(
    machine: &mut HotplugStateMachine,
    bus: &mut B,
    action: HotplugAction,
    reason: impl Into<String>,
    request_id: RequestId,
    audit_sink: &mut S,
) -> Result<HotplugPublishReport, EvaError>
where
    B: EventBus,
    S: AuditSink,
{
    let event = machine.apply(action, reason)?;
    let event_id = EventId::parse(&format!(
        "hotplug:{}:{}",
        event.device_id.as_str(),
        request_id.as_str()
    ))?;
    let payload = EventPayload::text(format!(
        "{{\"device_id\":\"{}\",\"action\":\"{}\",\"previous\":\"{}\",\"next\":\"{}\",\"reason\":\"{}\"}}",
        json_escape(event.device_id.as_str()),
        event.action.as_str(),
        event.previous.as_str(),
        event.next.as_str(),
        json_escape(&event.reason)
    ));
    let receipt = bus.publish(
        Event::new(event_id, event.topic.clone(), payload)
            .with_request_id(request_id.clone())
            .with_trace(TraceContext::default()),
    )?;
    let trace = TraceFields::default().with_request_id(request_id);
    audit_sink.record(
        AuditEvent::new(
            AuditAction::HardwareHotplugPublished,
            AuditOutcome::Ok,
            trace,
        )
        .with_message("hardware hotplug event published")
        .with_field("device_id", event.device_id.as_str())
        .with_field("topic", event.topic.as_str())
        .with_field("action", event.action.as_str()),
    )?;
    Ok(HotplugPublishReport { event, receipt })
}

fn ensure_os_permission(check: &OsPermissionCheck) -> Result<(), EvaError> {
    if check.granted {
        Ok(())
    } else {
        Err(
            EvaError::permission_denied("hardware OS permission is missing")
                .with_context("device_id", check.device_id.as_str())
                .with_context("bus", &check.bus)
                .with_context("permission", &check.permission),
        )
    }
}

fn record_lifecycle_audit<S>(
    audit_sink: &mut S,
    action: AuditAction,
    outcome: AuditOutcome,
    report: &DriverLifecycleReport,
) -> Result<(), EvaError>
where
    S: AuditSink,
{
    let trace = TraceFields::default().with_request_id(report.request_id.clone());
    audit_sink.record(
        AuditEvent::new(action, outcome, trace)
            .with_message("hardware driver lifecycle transition")
            .with_field("device_id", report.device_id.as_str())
            .with_field("driver_id", &report.driver_id)
            .with_field("state", report.state.as_str()),
    )
}

fn json_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeviceCandidate, DeviceHealth};
    use eva_config::policy::PolicyDocument;
    use eva_core::{AdapterId, ErrorKind};
    use eva_eventbus::InMemoryEventBus;
    use eva_observability::InMemoryAuditSink;
    use eva_policy::PolicyDomainSet;

    fn policy_gate() -> RuntimePolicyGate {
        let value: serde_yaml::Value = serde_yaml::from_str(
            r#"
hardware_policy:
  enabled: true
  allowed_buses:
    - usb
  denied_capabilities: []
  limits:
    max_timeout_ms: 5000
"#,
        )
        .unwrap();
        let document = PolicyDocument::try_from(value).unwrap();
        RuntimePolicyGate::new(PolicyDomainSet::from_documents(&[document]).unwrap())
    }

    fn registry() -> DeviceRegistry {
        let candidate =
            DeviceCandidate::for_adapter(AdapterId::parse("scale-main").unwrap(), "main-scale")
                .unwrap();
        DeviceRegistry::from_candidates(&[candidate]).unwrap()
    }

    fn start_request() -> DriverStartRequest {
        DriverStartRequest::new(
            DeviceId::parse("scale-main:main-scale").unwrap(),
            RequestId::parse("req-hardware-1").unwrap(),
            CapabilityName::parse("hardware.scale.read").unwrap(),
            "scale-main-simulated-driver",
        )
        .with_timeout_ms(1000)
    }

    #[test]
    fn lifecycle_checks_policy_os_permission_claim_and_audit() {
        let mut coordinator = HardwareLifecycleCoordinator::new(
            registry(),
            StaticOsPermissionProvider::granted("usb-device-access"),
        );
        let mut audit = InMemoryAuditSink::default();
        let device_id = DeviceId::parse("scale-main:main-scale").unwrap();

        let opened = coordinator
            .start_driver(start_request(), &policy_gate(), &mut audit)
            .unwrap();

        assert_eq!(opened.state, DriverLifecycleState::Opened);
        assert!(coordinator.active_session(&device_id).is_some());
        assert_eq!(
            coordinator.registry().get(&device_id).unwrap().health,
            DeviceHealth::Claimed
        );
        assert_eq!(audit.events[0].action, AuditAction::HardwareDriverStarted);
    }

    #[test]
    fn lifecycle_rejects_missing_os_permission_before_claim() {
        let mut coordinator = HardwareLifecycleCoordinator::new(
            registry(),
            StaticOsPermissionProvider::denied("usb-device-access"),
        );
        let mut audit = InMemoryAuditSink::default();
        let device_id = DeviceId::parse("scale-main:main-scale").unwrap();

        let error = coordinator
            .start_driver(start_request(), &policy_gate(), &mut audit)
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(
            coordinator.registry().get(&device_id).unwrap().health,
            DeviceHealth::Available
        );
        assert!(audit.events.is_empty());
    }

    #[test]
    fn lifecycle_stop_requires_matching_lease_and_releases() {
        let mut coordinator = HardwareLifecycleCoordinator::new(
            registry(),
            StaticOsPermissionProvider::granted("usb-device-access"),
        );
        let mut audit = InMemoryAuditSink::default();
        let device_id = DeviceId::parse("scale-main:main-scale").unwrap();
        coordinator
            .start_driver(start_request(), &policy_gate(), &mut audit)
            .unwrap();

        let error = coordinator
            .stop_driver(
                &device_id,
                &RequestId::parse("req-hardware-2").unwrap(),
                &mut audit,
            )
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);

        let stopped = coordinator
            .stop_driver(
                &device_id,
                &RequestId::parse("req-hardware-1").unwrap(),
                &mut audit,
            )
            .unwrap();
        assert_eq!(stopped.state, DriverLifecycleState::Stopped);
        assert_eq!(
            coordinator.registry().get(&device_id).unwrap().health,
            DeviceHealth::Available
        );
    }

    #[test]
    fn driver_crash_releases_lease_and_records_failed_audit() {
        let mut coordinator = HardwareLifecycleCoordinator::new(
            registry(),
            StaticOsPermissionProvider::granted("usb-device-access"),
        );
        let mut audit = InMemoryAuditSink::default();
        let device_id = DeviceId::parse("scale-main:main-scale").unwrap();
        coordinator
            .start_driver(start_request(), &policy_gate(), &mut audit)
            .unwrap();

        let crashed = coordinator
            .record_driver_crash(&device_id, "process exited", &mut audit)
            .unwrap();

        assert_eq!(crashed.state, DriverLifecycleState::Crashed);
        assert!(coordinator.active_session(&device_id).is_none());
        assert_eq!(
            coordinator.registry().get(&device_id).unwrap().health,
            DeviceHealth::Available
        );
        assert_eq!(audit.events[1].outcome, AuditOutcome::Failed);
    }

    #[test]
    fn hotplug_publish_emits_typed_eventbus_event() {
        let mut machine =
            HotplugStateMachine::new(DeviceId::parse("scale-main:main-scale").unwrap());
        let mut bus = InMemoryEventBus::new();
        let mut audit = InMemoryAuditSink::default();

        let report = publish_hotplug_event(
            &mut machine,
            &mut bus,
            HotplugAction::Reconnect,
            "device returned",
            RequestId::parse("req-hotplug-1").unwrap(),
            &mut audit,
        )
        .unwrap();

        assert_eq!(report.event.topic.as_str(), "/hardware/connected");
        assert_eq!(report.receipt.topic.as_str(), "/hardware/connected");
        assert_eq!(bus.receipts().len(), 1);
        assert_eq!(
            audit.events[0].action,
            AuditAction::HardwareHotplugPublished
        );
    }
}
