# eva-hardware / 硬件接入

更新时间：2026-07-04

![V1.x extension module flow](../assets/eva-extension-module-flow.svg)

`eva-hardware` 是 V1.3 的受控硬件接入层。它把硬件设备拆成“发现候选、可信身份、注册表、逻辑租约、driver binding、hotplug 状态机”几个可测试边界，并明确禁止 Lua、Agent 或 CLI 直接持有系统设备句柄、串口、USB、BLE、socket 或 vendor SDK raw I/O。

V1.3 的实现重点是建立可验证边界，而不是打开真实硬件：项目中的 `scale-main` 硬件 manifest 默认 `enabled: false`，CLI 可以发现、probe 和生成绑定计划，但不会打开设备文件。

## V1.3 已实现能力

| 功能域 | 当前状态 | 已实现行为 |
| --- | --- | --- |
| Discovery | 已完成 V1.3 | `discover_project_devices` 从 `transport: hardware` 的 Adapter manifest 中读取 bus、identity、match、protocol，生成 `DeviceCandidate`。Discovery 只返回候选，`handle_granted` 恒为 `false`。 |
| Identity / State | 已完成 V1.3 | `DeviceId`、`DeviceIdentity`、`DeviceBus`、`DeviceTrust`、`DeviceHealth` 提供稳定设备身份、总线、信任级别和健康状态。 |
| Registry | 已完成 V1.3 | `DeviceRegistry` 支持从候选注册可信设备，拒绝 rejected candidate，提供 `claim`、`release`、`list`、`get`，并阻止重复 claim。 |
| Driver binding | 已完成 V1.3 | `DriverBinding`、`DriverOperation`、`DriverOutput`、`HardwareDriver` 和 `SimulatedDriver` 把硬件能力封装成受控 trait。模拟 driver 的 audit 明确包含 `raw_io:false`。 |
| Hotplug | 已完成 V1.3 | `HotplugStateMachine` 将 insert/reconnect/remove/fail 转成稳定 Topic：`/hardware/connected`、`/hardware/disconnected`、`/hardware/failed`。 |
| Adapter bridge | 已完成 V1.3 | `eva-adapter` 的 hardware transport 通过 `DeviceRegistry` lease 调用 `SimulatedDriver`，完成后释放 lease，并输出 `transport:hardware`、`lease:released` 审计。 |
| CLI | 已完成 V1.3 | `eva hardware list/probe/bind` 可诊断硬件候选；`bind` 是 plan-first，`--apply` 在 V1.3 只校验逻辑计划，不打开 raw I/O。 |

## 数据流

V1.3 的硬件路径是：

1. `eva-config` 载入 `config/adapters/hardware/*.yaml`。
2. `eva-hardware::discover_project_devices` 读取硬件扩展字段，生成非授权 `DeviceCandidate`。
3. CLI `hardware list/probe` 展示候选、信任级别、健康状态、match 字段和拒绝原因。
4. `DeviceRegistry::from_candidates` 只注册非 rejected 候选。
5. `DeviceRegistry::claim` 为一次 request 生成独占 `DeviceLease`。
6. `SimulatedDriver` 通过 `HardwareDriver` trait 返回受控输出和审计记录。
7. Adapter hardware transport 释放 lease 后返回 `AdapterInvokeReport`。
8. 后续真实硬件 driver 必须复用同一条 registry + lease + driver binding 路径。

## Public API

| 类型/函数 | 说明 |
| --- | --- |
| `discover_project_devices(&ProjectConfig)` | 从项目配置发现硬件 Adapter 候选，不授予执行句柄。 |
| `DeviceCandidate` | 硬件发现候选，包含 identity、match 字段、protocol、health、source_path、rejected_reason。 |
| `DeviceIdentity` | 稳定设备身份，包含 `DeviceId`、logical name、device class、bus、adapter id、trust。 |
| `DeviceRegistry` | 已注册设备集合，负责 register、claim、release 和 claim 冲突检查。 |
| `DeviceLease` | request-scoped 独占租约；driver 调用必须持有 lease。 |
| `DriverBinding` | driver 与 capability、device class 的声明式绑定。 |
| `DriverOperation` / `DriverOutput` | driver 调用输入和输出 envelope。 |
| `HardwareDriver` | 真实或模拟 driver 必须实现的受控 trait。 |
| `SimulatedDriver` | V1.3 默认 driver，只返回模拟读取结果和 `raw_io:false` audit。 |
| `HotplugStateMachine` | 将硬件动作转换成健康状态和稳定硬件 Topic。 |

## CLI 验证入口

```powershell
cargo run -- hardware list --output json
cargo run -- hardware probe --adapter scale-main --output json
cargo run -- hardware bind --adapter scale-main --output json
```

`scale-main` 默认 disabled，因此 `hardware list` 会报告：

- `trust: rejected`
- `health: disconnected`
- `handle_granted: false`
- `rejected_reason: hardware adapter manifest is disabled`

`hardware bind --adapter scale-main` 会返回 `status: blocked`，并给出 plan steps 与风险提示。V1.3 即使传入 `--apply`，也只验证逻辑计划，不打开真实设备。

## 模块边界

`eva-hardware` 做：

- 发现和表示设备候选。
- 表达可信设备身份、总线、信任级别和健康状态。
- 管理设备注册、claim、release 和 lease 冲突。
- 提供 driver binding trait 和模拟 driver。
- 将 hotplug 动作映射为稳定 Topic。
- 为 AdapterRuntime 输出可审计的硬件调用边界。

`eva-hardware` 不做：

- 不让 Lua 或 Agent 直接打开串口、USB、BLE、网络 socket 或 vendor SDK。
- 不保存业务状态。
- 不绕过 AdapterRuntime 或 policy gate 调用硬件。
- 不在 V1.3 打开真实设备文件或执行不可逆设备操作。
- 不把 discovery candidate 当作授权 handle。

## 与其他模块的关系

| 模块 | 关系 |
| --- | --- |
| `eva-config` | 提供 Adapter manifest 和硬件扩展字段读取。 |
| `eva-adapter` | 通过 `hardware` transport 调用 registry、lease 和 driver binding。 |
| `eva-cli` | 暴露 `hardware list/probe/bind` 诊断和 plan-first 绑定命令。 |
| `eva-policy` | 后续版本继续扩展 raw I/O、设备路径、vendor SDK 权限约束。 |
| `eva-eventbus` | 后续版本可把 `HotplugEvent.topic` 发布到运行时事件流。 |

## 验证计划

```powershell
cargo test -p eva-hardware
cargo test -p eva-adapter
cargo test -p eva-cli
cargo run -- hardware list --output json
cargo run -- hardware probe --adapter scale-main --output json
cargo run -- hardware bind --adapter scale-main --output json
```

关键测试覆盖：

- discovery 返回非授权候选。
- registry claim/release 改变健康状态。
- registry 拒绝重复 claim。
- simulated driver 拒绝不匹配 capability。
- hotplug action 映射到稳定 Topic。
- Adapter hardware transport 输出 `raw_io:false` 并释放 lease。

## 后续范围

V1.3 没有集成真实 USB/串口/BLE/网络设备，也没有处理 OS 权限、vendor SDK、热插拔事件发布或真实 driver 生命周期。V1.5 发布加固阶段会继续补设备风险等级、模拟器回归、硬件集成测试隔离、metrics 和安全审计。

## English

`eva-hardware` owns the V1.3 controlled hardware boundary: discovery candidates, trusted identities, registry leases, driver bindings, hotplug state, and simulated driver invocation. Raw hardware I/O is not exposed to Lua, Agents, or CLI commands.
