# eva-config/src / 配置源码

![Eva module implementation roadmap](../../assets/eva-module-implementation-roadmap.svg)

本目录承载配置加载、manifest 解析、policy document 加载、routes 加载和 schema 路径辅助。V0.2 已完成最小配置闭环，V0.3 重点接 CLI 诊断；V1.9.1 起，项目加载会先执行 JSON Schema 子集校验并输出文件、字段、规则和建议；V1.10.1 起，hardware Adapter manifest 会解析为 typed hardware config；V1.14.5 起，顶层 `service_manager` 会解析为显式 typed config。

## 功能说明

| 文件/目录 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | `ProjectConfig` 聚合、加载入口、跨文件校验 | 已完成 | V0.2 |
| `eva_yaml.rs` | `config/eva.yaml`、`ConfigRoots` 和 `service_manager` typed config | 已完成 V1.14.5 | V0.2/V1.14.5 |
| `manifest/` | Agent/Adapter/Capability manifest；hardware typed driver config | 已完成 V1.10.1 | V0.2/V1.1/V1.10.1 |
| `policy.rs` | extensible policy document 加载 | 已完成 | V0.2 |
| `routes.rs` | Topic routes 配置加载 | 已完成 | V0.2/V0.4 |
| `schema.rs` | schema 路径、枚举辅助和 JSON Schema 子集校验 | 已完成 V1.9.1 | V0.2/V1.9.1 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 加载主配置和 manifest，复用 `eva-core` typed validation。 | `ProjectConfig` 可构造。 |
| 2 | 加载 policy document 和 routes。 | policy/routes 可进入后续模块。 |
| 3 | 执行跨文件一致性检查。 | 重复 ID、未知引用可拒绝。 |
| 4 | 接 CLI 和完整 JSON Schema validator。 | V1.9.1 诊断更完整。 |
| 5 | 解析 hardware typed config。 | V1.10.1 预留 USB/串口/BLE/socket/vendor SDK driver 配置。 |
| 6 | 解析 service-manager typed config。 | V1.14.5 要求启用时显式 kind 和 service name，并保留 fake/platform kind 边界。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Main config | `eva.yaml` 和 `service_manager` | 已完成 V1.14.5 | 后续随 V1.14.6 平台 adapter 增加 runtime binary/service name 校验。 |
| Manifests | Agent/Adapter/Capability 和 hardware typed config | 已完成 V1.10.1 | V1.10.2 扩展 OS 权限配置。 |
| Policy document | YAML domain map | 已完成 | 交给 `eva-policy` 解释。 |
| Routes | Topic route table | 已完成 | 接 Scheduler registry。 |
| Validation | schema + 跨文件检查 | 已完成 V1.9.1 | 已覆盖 schema rule 错误定位、Agent permission provider/capability 引用。 |
