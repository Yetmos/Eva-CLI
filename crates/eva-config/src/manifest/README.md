# Manifest Modules / Manifest 模块

![Eva module implementation roadmap](../../../assets/eva-module-implementation-roadmap.svg)

本目录承载 Agent、Adapter、Capability 三类 manifest 的配置结构和基础校验。V0.2 已完成最小字段解析，V1.10.1 已为 hardware Adapter 增加 typed bus/match/identity/protocol/hotplug/driver config。

## 功能说明

| 文件 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `mod.rs` | manifest 模块导出 | 已完成 | V0.2 |
| `agent.rs` | Agent ID、父子关系、脚本、订阅、权限字段 | 已完成 | V0.2/V0.4 |
| `adapter.rs` | Adapter ID、transport、capability provider 声明、hardware typed driver config | 已完成 V1.10.1 | V0.2/V1.1/V1.10.1 |
| `capability.rs` | Capability ID、runtime name、provider 引用 | 已完成 | V0.2/V0.4 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 为每类 manifest 定义 serde 结构和默认值。 | YAML 可加载。 |
| 2 | 使用 `eva-core` 类型校验 ID、TopicPattern、CapabilityName。 | 非法字段可拒绝。 |
| 3 | 在 `ProjectConfig` 层做跨 manifest 引用校验。 | provider、parent、route 引用可验证。 |
| 4 | 扩展 transport、schema、policy 和 hardware driver 字段。 | V1.x 外部能力和硬件 driver 可配置。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Agent manifest | Agent 注册前配置 | 已完成 | 接 AgentRuntime descriptor。 |
| Adapter manifest | Adapter 注册前配置和 hardware typed config | 已完成 V1.10.1 | 扩展真实 driver lifecycle 与 OS 权限字段。 |
| Capability manifest | Capability 注册前配置 | 已完成 | 接 CapabilityRegistry descriptor。 |
