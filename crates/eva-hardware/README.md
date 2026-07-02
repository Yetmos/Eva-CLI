# eva-hardware / 硬件接入

更新时间：2026-07-02

![V1.x extension module flow](../assets/eva-extension-module-flow.svg)

`eva-hardware` 负责设备发现、可信身份匹配、DeviceRegistry、driver binding、hotplug 状态机和硬件运行时状态。它不允许 Lua 直接进行 raw I/O，硬件调用必须通过 policy、AdapterRuntime 和 HardwareAdapter 边界。

## 当前模块功能说明

| 功能域 | 当前状态 | 目标行为 |
| --- | --- | --- |
| Discovery | 骨架 | 发现 USB、串口、BLE、网络等设备候选并做身份匹配。 |
| Registry | 骨架 | 维护 claimed device、logical binding、lease、health。 |
| Driver binding | 骨架 | 把设备能力封装成受控 driver trait。 |
| Hotplug | 骨架 | 处理插入、移除、重连、失败和恢复事件。 |
| State | 骨架 | 保存 hardware runtime state 和绑定状态。 |
| Adapter bridge | 未实现 | V1.3 通过 `eva-adapter` hardware transport 暴露受控调用。 |

## 模块边界

`eva-hardware` 做：

- 发现和表示设备候选。
- 管理设备 claim、release、health、hotplug。
- 提供 policy-controlled driver trait。
- 输出设备事件和审计记录。

`eva-hardware` 不做：

- 不让 Lua 或 Agent 直接打开串口、USB、BLE、网络 socket。
- 不保存业务状态。
- 不跳过 AdapterRuntime 调用硬件能力。
- 不执行不可逆设备操作，除非经过 plan/confirm/audit。

## 详细开发实施步骤

| 顺序 | 版本 | 步骤 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 1 | V1.3 | 定义 `DeviceCandidate`、trusted identity、device class、capability。 | `eva-core` | 候选可序列化输出。 |
| 2 | V1.3 | 实现 DeviceRegistry：claim、release、lease、health。 | `eva-policy` | 未授权设备不能 claim。 |
| 3 | V1.3 | 定义 driver trait 和 binding descriptor。 | manifest/policy | driver 不暴露 raw I/O。 |
| 4 | V1.3 | 实现 hotplug 状态机：insert/remove/reconnect/fail。 | `eva-eventbus` 后续 | 设备变化可进入事件流。 |
| 5 | V1.3 | 接 Adapter hardware transport。 | `eva-adapter` | Lua 只能通过 capability/adapter 调用设备。 |
| 6 | V1.5 | 增加设备风险等级、模拟器、硬件集成测试隔离。 | observability | 高风险设备操作可 plan 和 audit。 |

## 详细开发进度表

| 文件/模块 | 具体功能 | 当前进度 | 下一步 |
| --- | --- | --- | --- |
| `src/lib.rs` | 模块导出 | 骨架 | re-export discovery、registry、driver、hotplug、state。 |
| `src/discovery.rs` | 设备发现和身份匹配 | `RESPONSIBILITY` 占位 | 定义候选、身份、source、信任级别。 |
| `src/registry.rs` | claimed device registry | `RESPONSIBILITY` 占位 | 定义 claim/release/lease/health。 |
| `src/driver.rs` | policy-controlled driver binding | `RESPONSIBILITY` 占位 | 定义 driver trait、operation、result。 |
| `src/hotplug.rs` | hotplug 状态机 | `RESPONSIBILITY` 占位 | 定义事件、状态转换、失败恢复。 |
| `src/state.rs` | 硬件运行状态 | `RESPONSIBILITY` 占位 | 定义 device state snapshot 和 persistence hook。 |
| `src/README.md` | 源码目录说明 | 简略 | 补充文件职责和进度。 |
| 单元测试 | registry/hotplug | 未开始 | 覆盖 claim 冲突、移除、重连、未授权。 |

## 验证计划

| 阶段 | 命令 | 目标 |
| --- | --- | --- |
| V1.3 | `cargo test -p eva-hardware` | registry、driver、hotplug 可测。 |
| V1.3 | `cargo test -p eva-adapter` | hardware transport 只使用受控 device handle。 |
| V1.5 | hardware simulator tests | 无真实设备也能跑回归。 |

## English

`eva-hardware` owns device discovery, registry, driver binding, hotplug state, and hardware runtime state. Raw hardware I/O must not be exposed directly to Lua or Agents.
