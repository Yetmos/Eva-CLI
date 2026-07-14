//! 本模块提供 `state` 相关实现。
//! Hardware runtime state ownership.

use eva_core::{AdapterId, EvaError};

/// 说明本模块承担的架构职责。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "hardware runtime state ownership";

/// 定义 `DeviceBus` 可取的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceBus {
    /// 表示 `Usb` 枚举分支。
    Usb,
    /// 表示 `Serial` 枚举分支。
    Serial,
    /// 表示 `Ble` 枚举分支。
    Ble,
    /// 表示 `Socket` 枚举分支。
    Socket,
    /// 表示 `Network` 枚举分支。
    Network,
    /// 表示 `VendorSdk` 枚举分支。
    VendorSdk,
}

/// 定义 `DeviceTrust` 可取的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceTrust {
    /// 表示 `Manifest` 枚举分支。
    Manifest,
    /// 表示 `Observed` 枚举分支。
    Observed,
    /// 表示 `Rejected` 枚举分支。
    Rejected,
}

/// 定义 `DeviceHealth` 可取的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceHealth {
    /// 表示 `Candidate` 枚举分支。
    Candidate,
    /// 表示 `Available` 枚举分支。
    Available,
    /// 表示 `Claimed` 枚举分支。
    Claimed,
    /// 表示 `Disconnected` 枚举分支。
    Disconnected,
    /// 表示 `Failed` 枚举分支。
    Failed,
}

/// 表示经校验、可稳定比较和排序的设备标识。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(
    /// 保存去除首尾空白后的非空设备标识文本。
    String,
);

/// 表示 `DeviceIdentity` 数据结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    /// 记录 `id` 字段对应的值。
    pub id: DeviceId,
    /// 记录 `logical_name` 字段对应的值。
    pub logical_name: String,
    /// 记录 `device_class` 字段对应的值。
    pub device_class: String,
    /// 记录 `bus` 字段对应的值。
    pub bus: DeviceBus,
    /// 记录 `adapter_id` 字段对应的值。
    pub adapter_id: AdapterId,
    /// 记录 `trust` 字段对应的值。
    pub trust: DeviceTrust,
}

impl DeviceBus {
    /// 读取或解析 `parse` 所需的数据，失败时保留错误语义。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "usb" => Ok(Self::Usb),
            "serial" => Ok(Self::Serial),
            "ble" => Ok(Self::Ble),
            "socket" => Ok(Self::Socket),
            "network" => Ok(Self::Network),
            "vendor_sdk" => Ok(Self::VendorSdk),
            _ => Err(EvaError::unsupported("unsupported hardware bus").with_context("bus", value)),
        }
    }

    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Usb => "usb",
            Self::Serial => "serial",
            Self::Ble => "ble",
            Self::Socket => "socket",
            Self::Network => "network",
            Self::VendorSdk => "vendor_sdk",
        }
    }
}

impl DeviceTrust {
    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Observed => "observed",
            Self::Rejected => "rejected",
        }
    }
}

impl DeviceHealth {
    /// 读取或解析 `parse` 所需的数据，失败时保留错误语义。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        match value {
            "candidate" => Ok(Self::Candidate),
            "available" => Ok(Self::Available),
            "claimed" => Ok(Self::Claimed),
            "disconnected" => Ok(Self::Disconnected),
            "failed" => Ok(Self::Failed),
            _ => {
                Err(EvaError::unsupported("unsupported device health")
                    .with_context("health", value))
            }
        }
    }

    /// 将当前值按 `as_str` 约定的形式转换。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Available => "available",
            Self::Claimed => "claimed",
            Self::Disconnected => "disconnected",
            Self::Failed => "failed",
        }
    }
}

impl DeviceId {
    /// 读取或解析 `parse` 所需的数据，失败时保留错误语义。
    pub fn parse(value: &str) -> Result<Self, EvaError> {
        if value.trim().is_empty() {
            return Err(EvaError::invalid_argument("device id cannot be empty"));
        }
        if value.trim() != value || value.contains('/') || value.contains('\\') {
            return Err(EvaError::invalid_argument(
                "device id must be a stable slug",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    /// 将当前值按 `as_str` 约定的形式转换。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl DeviceIdentity {
    /// 创建并初始化当前类型的实例。
    pub fn new(
        id: DeviceId,
        logical_name: impl Into<String>,
        device_class: impl Into<String>,
        bus: DeviceBus,
        adapter_id: AdapterId,
        trust: DeviceTrust,
    ) -> Result<Self, EvaError> {
        let logical_name = logical_name.into();
        let device_class = device_class.into();
        if logical_name.trim().is_empty() || device_class.trim().is_empty() {
            return Err(EvaError::invalid_argument(
                "device logical name and class are required",
            ));
        }
        Ok(Self {
            id,
            logical_name,
            device_class,
            bus,
            adapter_id,
            trust,
        })
    }
}
