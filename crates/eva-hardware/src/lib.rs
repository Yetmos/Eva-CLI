//! Hardware discovery and hotplug boundary.

pub mod discovery;
pub mod driver;
pub mod hotplug;
pub mod registry;
pub mod state;

pub use discovery::{discover_project_devices, DeviceCandidate, HardwareDiscoveryReport};
pub use driver::{DriverBinding, DriverOperation, DriverOutput, HardwareDriver, SimulatedDriver};
pub use hotplug::{HotplugAction, HotplugEvent, HotplugStateMachine};
pub use registry::{DeviceLease, DeviceRegistry, RegisteredDevice};
pub use state::{DeviceBus, DeviceHealth, DeviceId, DeviceIdentity, DeviceTrust};
