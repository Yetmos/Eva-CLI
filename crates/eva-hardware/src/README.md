# eva-hardware/src / 硬件源码

更新时间：2026-07-04

![V1.x extension module flow](../../assets/eva-extension-module-flow.svg)

本目录承载 V1.3 硬件接入边界的源码。实现目标是让项目能够表达硬件设备、发现候选、注册可信设备、建立 request-scoped lease、通过受控 driver binding 调用硬件能力，并用 hotplug 状态机生成稳定事件 Topic。V1.3 不打开真实设备，也不暴露 raw I/O。

## 文件职责

| 文件 | 职责 | V1.3 状态 | 关键类型/函数 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出和公共 API 收敛 | 已完成 | re-export discovery、registry、driver、hotplug、state。 |
| `state.rs` | 硬件运行状态和稳定身份 | 已完成 | `DeviceBus`、`DeviceTrust`、`DeviceHealth`、`DeviceId`、`DeviceIdentity`。 |
| `discovery.rs` | device discovery 和可信身份匹配 | 已完成 | `discover_project_devices`、`DeviceCandidate`、`HardwareDiscoveryReport`。 |
| `registry.rs` | claimed device registry | 已完成 | `DeviceRegistry`、`RegisteredDevice`、`DeviceLease`。 |
| `driver.rs` | policy-controlled driver binding | 已完成 | `DriverBinding`、`DriverOperation`、`DriverOutput`、`HardwareDriver`、`SimulatedDriver`。 |
| `hotplug.rs` | hotplug state machine | 已完成 | `HotplugAction`、`HotplugEvent`、`HotplugStateMachine`。 |

## 关键不变量

- Discovery 只产生候选，不授权执行：`DeviceCandidate.handle_granted` 在 V1.3 恒为 `false`。
- 禁用的 hardware Adapter 会被标记为 `DeviceTrust::Rejected` 和 `DeviceHealth::Disconnected`。
- `DeviceRegistry::from_candidates` 不注册 rejected candidate。
- 同一设备一次只能被一个 request claim；重复 claim 返回 `Conflict`。
- Driver 调用必须持有 `DeviceLease`，且 operation capability 必须匹配 `DriverBinding.capability`。
- `SimulatedDriver` 只能返回模拟输出，audit 必须包含 `raw_io:false`。
- Hotplug Topic 只输出稳定公共路径：`/hardware/connected`、`/hardware/disconnected`、`/hardware/failed`。

## 源码级数据流

```text
ProjectConfig
  -> discover_project_devices
  -> DeviceCandidate[]
  -> DeviceRegistry::from_candidates
  -> DeviceRegistry::claim
  -> DeviceLease
  -> HardwareDriver::invoke
  -> DriverOutput + audit
  -> DeviceRegistry::release
```

CLI 的 `hardware list/probe/bind` 使用 discovery 输出作为诊断源；Adapter hardware transport 使用 registry + lease + driver binding 作为调用边界。

## 测试覆盖

| 文件 | 测试点 |
| --- | --- |
| `discovery.rs` | 项目硬件 manifest 能生成候选，且所有候选都不授予 handle。 |
| `registry.rs` | claim/release 改变健康状态；重复 claim 返回 `Conflict`。 |
| `driver.rs` | capability 不匹配时 simulated driver 返回 `PermissionDenied`。 |
| `hotplug.rs` | insert/remove/reconnect/fail 的状态迁移和 Topic 映射；available 状态下 reconnect 被拒绝。 |

运行：

```powershell
cargo test -p eva-hardware
```

## 后续开发约束

真实硬件 driver、OS device permission、vendor SDK、USB/serial/BLE/network I/O、hotplug event publish 都必须接在现有边界之后：先 discovery，再 registry claim，再 driver binding，再 adapter runtime audit。不要把设备路径、文件描述符、串口句柄或 SDK client 放进 Lua context、Agent state 或 discovery candidate。
