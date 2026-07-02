# eva-hardware/src / 硬件源码

![V1.x extension module flow](../../assets/eva-extension-module-flow.svg)

本目录承载设备发现、DeviceRegistry、driver binding、hotplug 和硬件状态。当前为骨架，V1.3 先建立不暴露 raw I/O 的硬件访问边界。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 骨架 | V1.3 |
| `discovery.rs` | device discovery 和可信身份匹配 | 骨架 | V1.3 |
| `registry.rs` | claimed device registry | 骨架 | V1.3 |
| `driver.rs` | policy-controlled driver binding | 骨架 | V1.3 |
| `hotplug.rs` | hotplug state machine | 骨架 | V1.3 |
| `state.rs` | hardware runtime state | 骨架 | V1.3 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义 DeviceCandidate、identity、device class。 | 候选可诊断。 |
| 2 | 实现 DeviceRegistry claim/release。 | 设备所有权可控。 |
| 3 | 定义 driver trait 和 operation envelope。 | 不暴露 raw I/O。 |
| 4 | 实现 hotplug 状态机并接 Adapter hardware transport。 | 设备调用受控执行。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Discovery | 候选和身份匹配 | 未实现 | 定义 trusted identity。 |
| Registry | claim/release/lease | 未实现 | 处理冲突和健康。 |
| Driver | operation trait | 未实现 | 设计权限 gate。 |
| Hotplug | 插拔和重连 | 未实现 | 定义状态转换。 |
| State | 运行状态 | 未实现 | 设计 snapshot。 |
