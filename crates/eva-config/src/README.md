# eva-config/src / 配置源码

![Eva module implementation roadmap](../../assets/eva-module-implementation-roadmap.svg)

本目录承载配置加载、manifest 解析、policy document 加载、routes 加载和 schema 路径辅助。V0.2 已完成最小配置闭环，V0.3 重点接 CLI 诊断。

## 功能说明

| 文件/目录 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | `ProjectConfig` 聚合、加载入口、跨文件校验 | 已完成 | V0.2 |
| `eva_yaml.rs` | `config/eva.yaml` 和 `ConfigRoots` | 已完成 | V0.2 |
| `manifest/` | Agent/Adapter/Capability manifest | 已完成 | V0.2/V1.1 |
| `policy.rs` | extensible policy document 加载 | 已完成 | V0.2 |
| `routes.rs` | Topic routes 配置加载 | 已完成 | V0.2/V0.4 |
| `schema.rs` | schema 路径和枚举辅助 | 已完成 | V0.2/V0.3 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 加载主配置和 manifest，复用 `eva-core` typed validation。 | `ProjectConfig` 可构造。 |
| 2 | 加载 policy document 和 routes。 | policy/routes 可进入后续模块。 |
| 3 | 执行跨文件一致性检查。 | 重复 ID、未知引用可拒绝。 |
| 4 | 接 CLI 和完整 JSON Schema validator。 | V0.3 诊断更完整。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Main config | `eva.yaml` | 已完成 | schema 错误定位。 |
| Manifests | Agent/Adapter/Capability | 已完成 | V1.1 扩展 Adapter/MCP 字段。 |
| Policy document | YAML domain map | 已完成 | 交给 `eva-policy` 解释。 |
| Routes | Topic route table | 已完成 | 接 Scheduler registry。 |
| Validation | 跨文件检查 | 已完成 | 扩展 permission/provider 检查。 |
