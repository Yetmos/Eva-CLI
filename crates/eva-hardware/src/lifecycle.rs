//! Hardware driver lifecycle coordination.

use crate::discovery::DeviceCandidate;
use crate::registry::{DeviceLease, DeviceRegistry, RegisteredDevice};
use crate::state::{DeviceHealth, DeviceId};
use crate::{HotplugAction, HotplugEvent, HotplugStateMachine};
use eva_core::{CapabilityName, EvaError, Event, EventId, EventPayload, RequestId, TraceContext};
use eva_eventbus::{EventBus, EventReceipt};
use eva_observability::{AuditAction, AuditEvent, AuditOutcome, AuditSink, TraceFields};
use eva_policy::{HighRiskAction, RuntimePolicyGate, RuntimePolicyRequest};
use std::collections::{BTreeMap, BTreeSet};

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "hardware driver lifecycle with OS permission, policy, lease, hotplug, and audit gates";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsPermissionCheck {
    pub device_id: DeviceId,
    pub bus: String,
    pub permission: String,
    pub granted: bool,
    pub os: String,
    pub user: String,
    pub source: String,
    pub device_path: String,
    pub remediation: Vec<String>,
    pub raw_device_path_exposed: bool,
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
pub struct PlatformOsPermissionProvider {
    os: String,
    user: String,
    default_granted: bool,
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
pub struct HardwareHotplugDeviceState {
    pub device_id: DeviceId,
    pub bus: String,
    pub health: DeviceHealth,
    pub source_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareHotplugSubscriberReport {
    pub status: String,
    pub watcher_kind: String,
    pub devices_seen: usize,
    pub events_published: Vec<HotplugPublishReport>,
    pub state: Vec<HardwareHotplugDeviceState>,
    pub raw_handles_exposed: bool,
    pub audit: Vec<String>,
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
            os: "test".to_owned(),
            user: "test-user".to_owned(),
            source: "static".to_owned(),
            device_path: safe_device_locator(device),
            remediation: permission_remediation(
                "test",
                device.identity.bus.as_str(),
                "test-user",
                &self.permission,
            ),
            raw_device_path_exposed: false,
        })
    }
}

impl PlatformOsPermissionProvider {
    pub fn current_process() -> Self {
        Self {
            os: std::env::consts::OS.to_owned(),
            user: current_user(),
            default_granted: false,
        }
    }

    pub fn new(os: impl Into<String>, user: impl Into<String>, default_granted: bool) -> Self {
        Self {
            os: os.into(),
            user: user.into(),
            default_granted,
        }
    }
}

impl Default for PlatformOsPermissionProvider {
    fn default() -> Self {
        Self::current_process()
    }
}

impl OsPermissionProvider for PlatformOsPermissionProvider {
    fn check(&self, device: &RegisteredDevice) -> Result<OsPermissionCheck, EvaError> {
        let bus = device.identity.bus.as_str().to_owned();
        let permission = permission_name_for_bus(device.identity.bus.as_str()).to_owned();
        Ok(OsPermissionCheck {
            device_id: device.identity.id.clone(),
            bus,
            permission: permission.clone(),
            granted: self.default_granted,
            os: self.os.clone(),
            user: self.user.clone(),
            source: permission_source(&self.os, device.identity.bus.as_str()),
            device_path: safe_device_locator(device),
            remediation: permission_remediation(
                &self.os,
                device.identity.bus.as_str(),
                &self.user,
                &permission,
            ),
            raw_device_path_exposed: false,
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
        self.record_crash(device_id, reason, "driver", audit_sink)
    }

    pub fn record_hotplug_watcher_crash<S>(
        &mut self,
        device_id: &DeviceId,
        reason: impl Into<String>,
        audit_sink: &mut S,
    ) -> Result<DriverLifecycleReport, EvaError>
    where
        S: AuditSink,
    {
        self.record_crash(device_id, reason, "hotplug_watcher", audit_sink)
    }

    fn record_crash<S>(
        &mut self,
        device_id: &DeviceId,
        reason: impl Into<String>,
        source: &'static str,
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
                format!("{source}:crashed:{reason}"),
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

pub fn run_hotplug_subscriber_once<B, S>(
    candidates: &[DeviceCandidate],
    previous_state: &[HardwareHotplugDeviceState],
    bus: &mut B,
    request_id_prefix: &str,
    audit_sink: &mut S,
) -> Result<HardwareHotplugSubscriberReport, EvaError>
where
    B: EventBus,
    S: AuditSink,
{
    RequestId::parse(request_id_prefix)?;
    let previous_by_id = previous_state
        .iter()
        .map(|state| (state.device_id.clone(), state))
        .collect::<BTreeMap<_, _>>();
    let mut sorted_candidates = candidates.to_vec();
    sorted_candidates.sort_by(|left, right| left.identity.id.cmp(&right.identity.id));

    let mut seen = BTreeSet::new();
    let mut state = Vec::new();
    let mut events_published = Vec::new();

    for candidate in &sorted_candidates {
        let device_id = candidate.identity.id.clone();
        let previous = previous_by_id.get(&device_id).copied();
        let previous_health = previous
            .map(|entry| entry.health)
            .unwrap_or(DeviceHealth::Disconnected);
        let current_health = candidate.health;
        seen.insert(device_id.clone());
        state.push(HardwareHotplugDeviceState {
            device_id: device_id.clone(),
            bus: candidate.identity.bus.as_str().to_owned(),
            health: current_health,
            source_path: candidate.source_path.clone(),
        });

        if let Some(action) =
            hotplug_action_for_transition(previous.is_some(), previous_health, current_health)
        {
            publish_subscriber_event(
                SubscriberEventInput {
                    device_id: &device_id,
                    previous_health,
                    action,
                    reason: transition_reason(previous_health, current_health),
                    request_id_prefix,
                },
                &mut events_published,
                bus,
                audit_sink,
            )?;
        }
    }

    for previous in previous_state {
        if seen.contains(&previous.device_id) {
            continue;
        }
        state.push(HardwareHotplugDeviceState {
            device_id: previous.device_id.clone(),
            bus: previous.bus.clone(),
            health: DeviceHealth::Disconnected,
            source_path: previous.source_path.clone(),
        });
        if previous.health != DeviceHealth::Disconnected {
            publish_subscriber_event(
                SubscriberEventInput {
                    device_id: &previous.device_id,
                    previous_health: previous.health,
                    action: HotplugAction::Remove,
                    reason: "manifest candidate removed".to_owned(),
                    request_id_prefix,
                },
                &mut events_published,
                bus,
                audit_sink,
            )?;
        }
    }

    state.sort_by(|left, right| left.device_id.cmp(&right.device_id));
    let event_count = events_published.len();
    Ok(HardwareHotplugSubscriberReport {
        status: "ready".to_owned(),
        watcher_kind: "manifest_snapshot".to_owned(),
        devices_seen: candidates.len(),
        events_published,
        state,
        raw_handles_exposed: false,
        audit: vec![
            "hardware_hotplug:subscriber_scan".to_owned(),
            format!("hardware_hotplug:events_published:{event_count}"),
            "hardware_hotplug:raw_handles_not_exposed".to_owned(),
        ],
    })
}

pub fn render_hotplug_subscriber_state(states: &[HardwareHotplugDeviceState]) -> String {
    let mut states = states.to_vec();
    states.sort_by(|left, right| left.device_id.cmp(&right.device_id));
    let mut output = String::from("version=1\n");
    for state in states {
        output.push_str(&format!(
            "device\t{}\t{}\t{}\t{}\n",
            encode_field(state.device_id.as_str()),
            state.health.as_str(),
            encode_field(&state.bus),
            encode_field(&state.source_path)
        ));
    }
    output
}

pub fn parse_hotplug_subscriber_state(
    data: &str,
) -> Result<Vec<HardwareHotplugDeviceState>, EvaError> {
    let mut version_seen = false;
    let mut states = Vec::new();
    for (index, line) in data.lines().enumerate() {
        let line_no = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        if line == "version=1" {
            version_seen = true;
            continue;
        }
        if !version_seen {
            return Err(EvaError::conflict("hardware hotplug state missing version")
                .with_context("line", line_no.to_string()));
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 5 || fields[0] != "device" {
            return Err(
                EvaError::conflict("hardware hotplug state record is invalid")
                    .with_context("line", line_no.to_string()),
            );
        }
        let device_id = DeviceId::parse(&decode_field(fields[1])?)
            .map_err(|error| error.with_context("line", line_no.to_string()))?;
        let health = DeviceHealth::parse(fields[2])
            .map_err(|error| error.with_context("line", line_no.to_string()))?;
        let bus = decode_field(fields[3])?;
        let source_path = decode_field(fields[4])?;
        states.push(HardwareHotplugDeviceState {
            device_id,
            bus,
            health,
            source_path,
        });
    }
    if !version_seen && !data.trim().is_empty() {
        return Err(EvaError::conflict("hardware hotplug state missing version"));
    }
    states.sort_by(|left, right| left.device_id.cmp(&right.device_id));
    Ok(states)
}

struct SubscriberEventInput<'a> {
    device_id: &'a DeviceId,
    previous_health: DeviceHealth,
    action: HotplugAction,
    reason: String,
    request_id_prefix: &'a str,
}

fn publish_subscriber_event<B, S>(
    input: SubscriberEventInput<'_>,
    events_published: &mut Vec<HotplugPublishReport>,
    bus: &mut B,
    audit_sink: &mut S,
) -> Result<(), EvaError>
where
    B: EventBus,
    S: AuditSink,
{
    let request_id = RequestId::parse(&format!(
        "{}-{}",
        input.request_id_prefix,
        events_published.len() + 1
    ))?;
    let mut machine = HotplugStateMachine::new(input.device_id.clone());
    machine.health = input.previous_health;
    let report = publish_hotplug_event(
        &mut machine,
        bus,
        input.action,
        input.reason,
        request_id,
        audit_sink,
    )?;
    events_published.push(report);
    Ok(())
}

fn hotplug_action_for_transition(
    has_previous: bool,
    previous: DeviceHealth,
    current: DeviceHealth,
) -> Option<HotplugAction> {
    if !has_previous {
        return match current {
            DeviceHealth::Available | DeviceHealth::Candidate => Some(HotplugAction::Insert),
            DeviceHealth::Disconnected => Some(HotplugAction::Remove),
            DeviceHealth::Failed => Some(HotplugAction::Fail),
            DeviceHealth::Claimed => None,
        };
    }
    if previous == current {
        return None;
    }
    match current {
        DeviceHealth::Available | DeviceHealth::Candidate => {
            if matches!(previous, DeviceHealth::Disconnected | DeviceHealth::Failed) {
                Some(HotplugAction::Reconnect)
            } else {
                Some(HotplugAction::Insert)
            }
        }
        DeviceHealth::Disconnected => Some(HotplugAction::Remove),
        DeviceHealth::Failed => Some(HotplugAction::Fail),
        DeviceHealth::Claimed => None,
    }
}

fn transition_reason(previous: DeviceHealth, current: DeviceHealth) -> String {
    format!(
        "manifest snapshot {} -> {}",
        previous.as_str(),
        current.as_str()
    )
}

fn ensure_os_permission(check: &OsPermissionCheck) -> Result<(), EvaError> {
    if check.granted {
        Ok(())
    } else {
        Err(
            EvaError::permission_denied("hardware OS permission is missing")
                .with_context("device_id", check.device_id.as_str())
                .with_context("bus", &check.bus)
                .with_context("permission", &check.permission)
                .with_context("os", &check.os)
                .with_context("source", &check.source)
                .with_context("remediation", check.remediation.join(" | ")),
        )
    }
}

fn current_user() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn permission_name_for_bus(bus: &str) -> &'static str {
    match bus {
        "usb" => "usb-device-access",
        "serial" => "serial-port-access",
        "ble" => "bluetooth-device-access",
        "socket" | "network" => "network-socket-access",
        "vendor_sdk" => "vendor-sdk-access",
        _ => "hardware-device-access",
    }
}

fn permission_source(os: &str, bus: &str) -> String {
    match os {
        "windows" => "Windows device interface ACL or driver installation".to_owned(),
        "linux" if bus == "serial" => "Linux udev rule and dialout group".to_owned(),
        "linux" => "Linux udev rule and device node permission".to_owned(),
        "macos" if bus == "ble" => "macOS Bluetooth permission".to_owned(),
        "macos" => "macOS device entitlement or device node permission".to_owned(),
        _ => "platform hardware permission provider".to_owned(),
    }
}

fn permission_remediation(os: &str, bus: &str, user: &str, permission: &str) -> Vec<String> {
    let mut remediation = vec![format!(
        "grant {permission} to user {user} before starting a hardware driver"
    )];
    match os {
        "windows" => remediation.push(
            "install the vendor driver and grant the service account access to the device interface"
                .to_owned(),
        ),
        "linux" if bus == "serial" => remediation.push(
            "add the service account to the serial device group and install a udev rule".to_owned(),
        ),
        "linux" => remediation.push(
            "install a udev rule that matches the device identity and grants least-privilege access"
                .to_owned(),
        ),
        "macos" if bus == "ble" => remediation.push(
            "grant Bluetooth permission to the runtime process before enabling the driver".to_owned(),
        ),
        "macos" => remediation.push(
            "grant device entitlement or device node access to the runtime process".to_owned(),
        ),
        _ => remediation.push(
            "configure an explicit platform permission provider before enabling this driver"
                .to_owned(),
        ),
    }
    remediation
}

fn safe_device_locator(device: &RegisteredDevice) -> String {
    format!(
        "logical://hardware/{}/{}",
        device.identity.bus.as_str(),
        device.identity.id.as_str()
    )
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

fn encode_field(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn decode_field(value: &str) -> Result<String, EvaError> {
    if !value.len().is_multiple_of(2) {
        return Err(EvaError::conflict(
            "hardware hotplug encoded field has odd length",
        ));
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for index in (0..value.len()).step_by(2) {
        let byte = u8::from_str_radix(&value[index..index + 2], 16).map_err(|_| {
            EvaError::conflict("hardware hotplug encoded field is not hex")
                .with_context("offset", index.to_string())
        })?;
        bytes.push(byte);
    }
    String::from_utf8(bytes).map_err(|error| {
        EvaError::conflict("hardware hotplug encoded field is not utf-8")
            .with_context("utf8_error", error.to_string())
    })
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
    fn platform_permission_provider_reports_safe_diagnostics() {
        let registry = registry();
        let device = registry
            .get(&DeviceId::parse("scale-main:main-scale").unwrap())
            .unwrap();
        let provider = PlatformOsPermissionProvider::new("linux", "eva", false);

        let check = provider.check(device).unwrap();

        assert!(!check.granted);
        assert_eq!(check.os, "linux");
        assert_eq!(check.user, "eva");
        assert_eq!(check.permission, "usb-device-access");
        assert!(check.source.contains("udev"));
        assert!(check.device_path.starts_with("logical://hardware/usb/"));
        assert!(!check.raw_device_path_exposed);
        assert!(check.remediation.iter().any(|item| item.contains("udev")));
    }

    #[test]
    fn platform_permission_denial_blocks_before_driver_claim() {
        let mut coordinator = HardwareLifecycleCoordinator::new(
            registry(),
            PlatformOsPermissionProvider::new("windows", "eva-service", false),
        );
        let mut audit = InMemoryAuditSink::default();
        let device_id = DeviceId::parse("scale-main:main-scale").unwrap();

        let error = coordinator
            .start_driver(start_request(), &policy_gate(), &mut audit)
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(error
            .context()
            .entries()
            .iter()
            .any(|(key, value)| key == "source" && value.contains("Windows")));
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

    #[test]
    fn hotplug_subscriber_publishes_logical_state_without_raw_handles() {
        let mut bus = InMemoryEventBus::new();
        let mut audit = InMemoryAuditSink::default();
        let candidate =
            DeviceCandidate::for_adapter(AdapterId::parse("scale-main").unwrap(), "main-scale")
                .unwrap();

        let report = run_hotplug_subscriber_once(
            std::slice::from_ref(&candidate),
            &[],
            &mut bus,
            "req-hotplug-scan",
            &mut audit,
        )
        .unwrap();

        assert_eq!(report.status, "ready");
        assert!(!report.raw_handles_exposed);
        assert_eq!(report.devices_seen, 1);
        assert_eq!(report.events_published.len(), 1);
        assert_eq!(
            report.events_published[0].event.action,
            HotplugAction::Insert
        );
        assert_eq!(bus.receipts().len(), 1);
        assert_eq!(bus.receipts()[0].topic.as_str(), "/hardware/connected");
        assert!(audit
            .events
            .iter()
            .any(|event| event.action == AuditAction::HardwareHotplugPublished));

        let rerun = run_hotplug_subscriber_once(
            &[candidate],
            &report.state,
            &mut bus,
            "req-hotplug-rerun",
            &mut audit,
        )
        .unwrap();

        assert!(rerun.events_published.is_empty());
        assert_eq!(bus.receipts().len(), 1);
    }

    #[test]
    fn hotplug_subscriber_reports_remove_reconnect_and_fail_transitions() {
        let mut bus = InMemoryEventBus::new();
        let mut audit = InMemoryAuditSink::default();
        let mut candidate =
            DeviceCandidate::for_adapter(AdapterId::parse("scale-main").unwrap(), "main-scale")
                .unwrap();
        let previous = vec![HardwareHotplugDeviceState {
            device_id: candidate.identity.id.clone(),
            bus: candidate.identity.bus.as_str().to_owned(),
            health: DeviceHealth::Available,
            source_path: candidate.source_path.clone(),
        }];

        candidate.health = DeviceHealth::Disconnected;
        let removed = run_hotplug_subscriber_once(
            &[candidate.clone()],
            &previous,
            &mut bus,
            "req-hotplug-remove",
            &mut audit,
        )
        .unwrap();
        assert_eq!(
            removed.events_published[0].event.action,
            HotplugAction::Remove
        );

        candidate.health = DeviceHealth::Available;
        let reconnected = run_hotplug_subscriber_once(
            &[candidate.clone()],
            &removed.state,
            &mut bus,
            "req-hotplug-reconnect",
            &mut audit,
        )
        .unwrap();
        assert_eq!(
            reconnected.events_published[0].event.action,
            HotplugAction::Reconnect
        );

        candidate.health = DeviceHealth::Failed;
        let failed = run_hotplug_subscriber_once(
            &[candidate],
            &reconnected.state,
            &mut bus,
            "req-hotplug-fail",
            &mut audit,
        )
        .unwrap();
        assert_eq!(failed.events_published[0].event.action, HotplugAction::Fail);
    }

    #[test]
    fn hotplug_subscriber_state_round_trips() {
        let state = vec![HardwareHotplugDeviceState {
            device_id: DeviceId::parse("scale-main:main-scale").unwrap(),
            bus: "usb".to_owned(),
            health: DeviceHealth::Disconnected,
            source_path: "C:\\tmp\\scale-main.yaml".to_owned(),
        }];

        let rendered = render_hotplug_subscriber_state(&state);
        let parsed = parse_hotplug_subscriber_state(&rendered).unwrap();

        assert_eq!(parsed, state);
    }

    #[test]
    fn hotplug_watcher_crash_releases_active_lease() {
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
            .record_hotplug_watcher_crash(&device_id, "watcher channel closed", &mut audit)
            .unwrap();

        assert_eq!(crashed.state, DriverLifecycleState::Crashed);
        assert!(crashed
            .audit
            .iter()
            .any(|entry| entry == "hotplug_watcher:crashed:watcher channel closed"));
        assert!(coordinator.active_session(&device_id).is_none());
        assert_eq!(
            coordinator.registry().get(&device_id).unwrap().health,
            DeviceHealth::Available
        );
    }
}
