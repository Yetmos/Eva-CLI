# eva-hardware/src / 硬件源码

更新时间：2026-07-08

![V1.x extension module flow](../../assets/eva-extension-module-flow.svg)

本目录承载硬件接入边界的源码。实现目标是让项目能够表达硬件设备、发现候选、注册可信设备、建立 request-scoped lease、通过受控 driver registry 和 driver lifecycle 调用硬件能力，并用 hotplug 状态机生成稳定事件 Topic。V1.10.2 不打开真实设备，也不暴露 raw I/O。

## 文件职责

| 文件 | 职责 | V1.3 状态 | 关键类型/函数 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出和公共 API 收敛 | 已完成 | re-export discovery、registry、driver、hotplug、state。 |
| `state.rs` | 硬件运行状态和稳定身份 | 已完成 | `DeviceBus`、`DeviceTrust`、`DeviceHealth`、`DeviceId`、`DeviceIdentity`。 |
| `discovery.rs` | device discovery 和可信身份匹配 | 已完成 V1.10.1 | `discover_project_devices`、`DeviceCandidate`、`HardwareDiscoveryReport`；从 `eva-config` typed hardware config 读取 bus、identity、match、protocol。 |
| `registry.rs` | claimed device registry | 已完成 | `DeviceRegistry`、`RegisteredDevice`、`DeviceLease`。 |
| `driver.rs` | policy-controlled driver registry 和 binding | 已完成 V1.10.1 | `DriverBinding`、`DriverOperation`、`DriverOutput`、`HardwareDriver`、`HardwareDriverRegistry`、`SimulatedDriver`、`run_simulator_contract_suite`。 |
| `lifecycle.rs` | driver lifecycle、OS permission、hotplug publish、audit | 已完成 V1.10.2 | `HardwareLifecycleCoordinator`、`StaticOsPermissionProvider`、`publish_hotplug_event`、`DriverLifecycleReport`。 |
| `hotplug.rs` | hotplug state machine | 已完成 | `HotplugAction`、`HotplugEvent`、`HotplugStateMachine`。 |

## 关键不变量

- Discovery 只产生候选，不授权执行：`DeviceCandidate.handle_granted` 在 V1.3 恒为 `false`。
- 禁用的 hardware Adapter 会被标记为 `DeviceTrust::Rejected` 和 `DeviceHealth::Disconnected`。
- `DeviceRegistry::from_candidates` 不注册 rejected candidate。
- 同一设备一次只能被一个 request claim；重复 claim 返回 `Conflict`。
- Driver 调用必须持有 `DeviceLease`，且 operation capability 必须匹配 `DriverBinding.capability`。
- `SimulatedDriver` 只能返回模拟输出，audit 必须包含 `raw_io:false`。
- `HardwareDriverRegistry` 只调用已注册 driver id，并拒绝重复 driver 注册。
- simulator contract suite 必须证明无 raw handle 暴露、无 raw I/O、capability mismatch 被拒绝。
- Driver lifecycle start 必须先通过 `RuntimePolicyGate` 和 OS permission check，再 claim lease 并进入 opened。
- Driver stop 必须使用匹配 request id；crash path 必须释放 lease 并写 failed audit。
- Hotplug publish 必须通过 `EventBus` 发送 typed payload，并写 `hardware.hotplug.published` audit。
- Hotplug Topic 只输出稳定公共路径：`/hardware/connected`、`/hardware/disconnected`、`/hardware/failed`。

## 源码级数据流

```text
ProjectConfig
  -> discover_project_devices
  -> DeviceCandidate[]
  -> DeviceRegistry::from_candidates
  -> RuntimePolicyGate + OsPermissionProvider
  -> DeviceRegistry::claim
  -> DeviceLease
  -> HardwareLifecycleCoordinator::start_driver
  -> HardwareDriverRegistry::invoke
  -> HardwareDriver::invoke
  -> DriverOutput + audit
  -> HardwareLifecycleCoordinator::stop_driver
  -> DeviceRegistry::release
```

CLI 的 `hardware list/probe/bind` 使用 discovery 输出作为诊断源；Adapter hardware transport 使用 device registry + lease + driver registry + driver binding 作为调用边界。

## 测试覆盖

| 文件 | 测试点 |
| --- | --- |
| `discovery.rs` | 项目硬件 manifest 能生成候选，且所有候选都不授予 handle。 |
| `registry.rs` | claim/release 改变健康状态；重复 claim 返回 `Conflict`。 |
| `driver.rs` | capability 不匹配时 simulated driver 返回 `PermissionDenied`；driver registry 调用已注册 simulator；simulator contract suite 验证无 raw I/O/raw handle。 |
| `lifecycle.rs` | policy/OS permission/claim/audit 顺序；OS permission 缺失不 claim；stop request id 必须匹配 lease；crash 释放 lease；hotplug publish 写入 EventBus。 |
| `hotplug.rs` | insert/remove/reconnect/fail 的状态迁移和 Topic 映射；available 状态下 reconnect 被拒绝。 |

运行：

```powershell
cargo test -p eva-hardware
```

## 后续开发约束

真实硬件 driver、OS device permission provider、vendor SDK、USB/serial/BLE/socket I/O、hotplug runtime subscriber 都必须接在现有边界之后：先 discovery，再 policy/OS permission，再 registry claim，再 lifecycle open，再 driver registry，再 driver binding，再 adapter runtime audit。不要把设备路径、文件描述符、串口句柄或 SDK client 放进 Lua context、Agent state 或 discovery candidate。
