//! Hardware discovery and hotplug boundary.

pub mod discovery;
pub mod driver;
pub mod hotplug;
pub mod lifecycle;
pub mod registry;
pub mod state;

pub use discovery::{discover_project_devices, DeviceCandidate, HardwareDiscoveryReport};
pub use driver::{
    run_simulator_contract_suite, DriverBinding, DriverOperation, DriverOutput, HardwareDriver,
    HardwareDriverRegistry, SimulatedDriver, SimulatorContractReport,
};
pub use hotplug::{HotplugAction, HotplugEvent, HotplugStateMachine};
pub use lifecycle::{
    publish_hotplug_event, ActiveDriverSession, DriverLifecycleReport, DriverLifecycleState,
    DriverStartRequest, HardwareLifecycleCoordinator, HotplugPublishReport, OsPermissionCheck,
    OsPermissionProvider, StaticOsPermissionProvider,
};
pub use registry::{DeviceLease, DeviceRegistry, RegisteredDevice};
pub use state::{DeviceBus, DeviceHealth, DeviceId, DeviceIdentity, DeviceTrust};
