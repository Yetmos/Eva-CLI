//! 本模块提供 `lib` 相关实现。
//! Hardware discovery and hotplug boundary.

/// 声明 `discovery` 子模块。
pub mod discovery;
/// 声明 `driver` 子模块。
pub mod driver;
/// 声明 `hotplug` 子模块。
pub mod hotplug;
/// 声明 `lifecycle` 子模块。
pub mod lifecycle;
/// 声明 `registry` 子模块。
pub mod registry;
/// 声明 `state` 子模块。
pub mod state;

pub use discovery::{discover_project_devices, DeviceCandidate, HardwareDiscoveryReport};
pub use driver::{
    run_simulator_contract_suite, DriverBinding, DriverOperation, DriverOutput, HardwareDriver,
    HardwareDriverRegistry, SimulatedDriver, SimulatorContractReport,
};
pub use hotplug::{HotplugAction, HotplugEvent, HotplugStateMachine};
pub use lifecycle::{
    parse_hotplug_subscriber_state, publish_hotplug_event, render_hotplug_subscriber_state,
    run_hotplug_subscriber_once, ActiveDriverSession, DriverLifecycleReport, DriverLifecycleState,
    DriverStartRequest, HardwareHotplugDeviceState, HardwareHotplugSubscriberReport,
    HardwareLifecycleCoordinator, HotplugPublishReport, OsPermissionCheck, OsPermissionProvider,
    PlatformOsPermissionProvider, StaticOsPermissionProvider,
};
pub use registry::{DeviceLease, DeviceRegistry, RegisteredDevice};
pub use state::{DeviceBus, DeviceHealth, DeviceId, DeviceIdentity, DeviceTrust};
