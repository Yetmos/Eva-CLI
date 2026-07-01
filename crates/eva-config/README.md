# eva-config / 配置加载

更新时间：2026-07-01

![eva-config validation flow](assets/eva-config-validation-flow.svg)

`eva-config` 负责把人工维护的项目配置转换成运行时可以信任的结构化输入。它加载 `eva.yaml`、Agent/Adapter/Capability manifest、routes、policy 和 schema 相关配置，并把其中稳定字段校验为 `eva-core` 中定义的 Topic、ID、Capability、Error 等基础契约。

这个模块是 `eva-core` 之后最应该优先落地的下游模块。只有配置和 manifest 能被稳定读取、归一化和验证，后续 `eva validate`、`eva-runtime`、`eva-eventbus`、`eva-scheduler`、`eva-agent` 和 `eva-adapter` 才有可靠输入。

## 模块边界

| 范围 | `eva-config` 负责 | `eva-config` 不负责 |
| --- | --- | --- |
| 主配置 | 读取 `eva.yaml`，解析 runtime/config roots 等稳定字段 | 启动 runtime、创建服务、修改进程状态 |
| Manifest | 读取 Agent、Adapter、Capability manifest 并归一化 | 执行 Agent、调用 Adapter、选择真实 provider |
| Schema | 维护 schema 路径、字段对齐测试、后续 schema 校验入口 | 在第一阶段引入完整 JSON Schema 引擎 |
| 契约校验 | 用 `eva-core` 校验 ID、TopicPattern、CapabilityName | 重新定义底层 ID、Topic、Capability 规则 |
| 错误 | 返回结构化 `EvaError` | 打印 CLI 输出、写审计日志、决定重试策略 |
| Policy | 加载 policy 配置入口和文件位置 | 做最终权限合并或授权裁决 |

## 当前状态

| 文件/范围 | 当前状态 | 下一步 |
| --- | --- | --- |
| `src/eva_yaml.rs` | 只有主配置加载职责占位 | 定义 `EvaConfig`、`RuntimeConfig`、`ConfigRoots` |
| `src/manifest/agent.rs` | 只有 Agent manifest 职责占位 | 定义 `AgentManifest` 并校验 `AgentId`、`TopicPattern` |
| `src/manifest/adapter.rs` | 只有 Adapter manifest 职责占位 | 定义 `AdapterManifest`、`AdapterTransport` |
| `src/manifest/capability.rs` | 只有 Capability manifest 职责占位 | 定义 `CapabilityManifest`、`CapabilityKind` |
| `src/schema.rs` | 尚未形成 schema 加载或校验 API | 定义 schema root 和 schema 对齐测试入口 |
| `Cargo.toml` | 目前只依赖 `eva-core` | 第一阶段如需 YAML 反序列化，应单独评估并保持依赖最小 |
| `config/` | 已有示例配置和 JSON Schema | 作为加载测试与字段对齐测试样本 |

## 输入输出

| 输入 | 来源 | 输出 |
| --- | --- | --- |
| `config/eva.yaml` | 项目主配置 | `EvaConfig`、配置根路径、schema 根路径 |
| `config/agents/*/agent.yaml` | Agent manifest | `AgentManifest`、订阅 TopicPattern、脚本入口 |
| `config/adapters/*.yaml` | Adapter manifest | `AdapterManifest`、transport、capability 声明 |
| `config/capabilities/*.yaml` | Capability manifest | `CapabilityManifest`、runtime capability name |
| `config/policies/*.yaml` | Policy 配置 | policy 文件入口，后续交给 `eva-policy` |
| `config/routes/topics.yaml` | Topic routes | route 文件入口，后续交给 `eva-scheduler` |

## 第一阶段目标

第一阶段只做最小配置加载与 manifest 验证链路：

```text
project root
  -> config/eva.yaml
  -> EvaConfig
  -> config roots
  -> AgentManifest / AdapterManifest / CapabilityManifest
  -> eva-core typed validation
  -> ProjectConfig
  -> eva validate
```

完成后，`eva-config` 应能回答这些问题：

| 问题 | 判断依据 |
| --- | --- |
| 主配置是否存在且可解析 | `load_eva_config(path)` 成功 |
| 配置根路径是否能归一化 | `ConfigRoots` 字段完整且路径合法 |
| Agent manifest 是否可注册 | `AgentId` 合法，script 存在声明，subscriptions 可解析 |
| Adapter manifest 是否可注册 | `AdapterId` 合法，transport 属于受支持枚举 |
| Capability manifest 是否可注册 | `CapabilityId` 合法，runtime capability 是合法 `CapabilityName` |
| 配置错误是否可诊断 | 返回 `EvaError`，包含 kind、message 和必要上下文 |

## 类型计划

| 类型 | 所属文件 | 关键字段 | 用途 |
| --- | --- | --- | --- |
| `EvaConfig` | `src/eva_yaml.rs` | `runtime`、`eventbus`、`scheduler`、`observability`、`config` | 主配置根对象 |
| `RuntimeConfig` | `src/eva_yaml.rs` | `env`、`workspace`、`data_dir`、`script_dir`、`adapter_dir`、`hot_reload` | Runtime builder 的配置输入 |
| `ConfigRoots` | `src/eva_yaml.rs` | `agent_dir`、`adapter_dir`、`capability_dir`、`policy_dir`、`route_file`、`schema_dir` | 分散配置文件的发现入口 |
| `AgentManifest` | `src/manifest/agent.rs` | `id`、`enabled`、`script`、`subscriptions`、`permissions` | Agent 注册前的配置契约 |
| `AdapterManifest` | `src/manifest/adapter.rs` | `id`、`name`、`version`、`enabled`、`transport`、`capabilities` | Adapter 注册前的配置契约 |
| `CapabilityManifest` | `src/manifest/capability.rs` | `id`、`name`、`version`、`enabled`、`kind`、`capability` | Capability 注册前的配置契约 |
| `ProjectConfig` | `src/lib.rs` 或 `src/eva_yaml.rs` | 主配置与已加载 manifest 集合 | `eva validate` 和 runtime builder 的最小输入 |

## eva-core 映射

| 配置字段 | 目标类型 | 校验规则来源 |
| --- | --- | --- |
| `agent.id` | `eva_core::AgentId` | 稳定 ID newtype |
| `agent.parent` | `Option<eva_core::AgentId>` | 稳定 ID newtype |
| `agent.children[]` | `Vec<eva_core::AgentId>` | 稳定 ID newtype |
| `agent.subscriptions[]` | `Vec<eva_core::TopicPattern>` | TopicPattern parser |
| `adapter.id` | `eva_core::AdapterId` | 稳定 ID newtype |
| `adapter.capabilities[]` | `Vec<eva_core::CapabilityName>` | CapabilityName parser |
| `capability.id` | `eva_core::CapabilityId` | 稳定 ID newtype |
| `capability.capability` | `eva_core::CapabilityName` | CapabilityName parser |
| 加载/校验错误 | `eva_core::EvaError` | 结构化错误模型 |

## 公开 API 计划

| API | 输入 | 输出 | 说明 |
| --- | --- | --- | --- |
| `load_eva_config` | `impl AsRef<Path>` | `Result<EvaConfig, EvaError>` | 读取并校验主配置 |
| `load_agent_manifest` | `impl AsRef<Path>` | `Result<AgentManifest, EvaError>` | 读取单个 Agent manifest |
| `load_adapter_manifest` | `impl AsRef<Path>` | `Result<AdapterManifest, EvaError>` | 读取单个 Adapter manifest |
| `load_capability_manifest` | `impl AsRef<Path>` | `Result<CapabilityManifest, EvaError>` | 读取单个 Capability manifest |
| `load_project_config` | `impl AsRef<Path>` | `Result<ProjectConfig, EvaError>` | 从项目根目录加载最小配置集合 |
| `validate_project_config` | `&ProjectConfig` | `Result<(), EvaError>` | 做跨文件一致性检查 |

第一阶段 API 可以同步、阻塞、无 async。配置加载是启动前动作，不需要提前引入 runtime 或 Tokio。

## 实施切片

| 顺序 | 切片 | 主要文件 | 验收 |
| --- | --- | --- | --- |
| 1 | 添加最小 YAML 反序列化能力 | `Cargo.toml` | 依赖理由清楚，`cargo check -p eva-config` 通过 |
| 2 | 实现主配置类型和加载 | `src/eva_yaml.rs` | 能加载 `config/eva.yaml` |
| 3 | 实现 Agent manifest | `src/manifest/agent.rs` | 能加载 `config/agents/*/agent.yaml` 并拒绝非法订阅 |
| 4 | 实现 Adapter manifest | `src/manifest/adapter.rs` | 能加载 `config/adapters/*.yaml` 并拒绝非法 capability |
| 5 | 实现 Capability manifest | `src/manifest/capability.rs` | 能加载 `config/capabilities/*.yaml` 并拒绝非法 capability name |
| 6 | 实现项目级加载 | `src/lib.rs`、`src/eva_yaml.rs` | `load_project_config` 能汇总主配置和 manifest |
| 7 | 增加 schema 对齐测试 | `src/schema.rs`、测试文件 | required 字段、枚举字段和样例配置保持一致 |
| 8 | 为 CLI 暴露验证入口 | `eva-cli` 后续接入 | `eva validate --config config/eva.yaml` 有稳定底层 API |

## 错误语义

| 场景 | 建议 `ErrorKind` | message 要包含 |
| --- | --- | --- |
| 配置文件不存在 | `NotFound` | 文件路径和配置类别 |
| YAML 解析失败 | `InvalidArgument` | 文件路径、解析位置或字段名 |
| 必填字段缺失 | `InvalidArgument` | manifest 类型和字段名 |
| ID、TopicPattern、CapabilityName 非法 | `InvalidArgument` | 字段名、非法值、规则来源 |
| 跨文件引用不存在 | `NotFound` | 引用方、被引用 ID |
| 重复 ID | `Conflict` | 重复 ID 和涉及文件 |
| 当前阶段不支持的配置 | `Unsupported` | 字段名和后续归属模块 |

## 测试计划

| 测试名 | 覆盖内容 |
| --- | --- |
| `load_eva_config_accepts_sample_config` | 示例 `config/eva.yaml` 可加载 |
| `load_eva_config_rejects_missing_required_runtime` | 主配置缺少必填 runtime 字段会失败 |
| `load_agent_manifest_accepts_sample_agent` | 示例 Agent manifest 可加载 |
| `load_agent_manifest_rejects_invalid_agent_id` | 非法 Agent ID 会失败 |
| `load_agent_manifest_rejects_invalid_subscription_pattern` | 非法 TopicPattern 会失败 |
| `load_adapter_manifest_accepts_sample_adapter` | 示例 Adapter manifest 可加载 |
| `load_adapter_manifest_rejects_unknown_transport` | 未知 transport 会失败 |
| `load_adapter_manifest_rejects_invalid_capability_name` | 非法 capability name 会失败 |
| `load_capability_manifest_accepts_sample_capability` | 示例 Capability manifest 可加载 |
| `load_capability_manifest_rejects_invalid_runtime_capability` | 非法 runtime capability 会失败 |
| `project_config_loads_all_config_roots` | 项目级加载能汇总主配置和 manifest |

## 验收标准

| 类别 | 标准 |
| --- | --- |
| 编译 | `cargo check -p eva-config` 通过 |
| 单元测试 | `cargo test -p eva-config` 通过 |
| Workspace 影响 | `cargo check --workspace` 通过 |
| 样例配置 | 仓库内 `config/eva.yaml` 和示例 manifest 均可加载 |
| 契约复用 | 不在 `eva-config` 重复定义 Topic、ID、Capability 校验规则 |
| 错误输出 | 加载失败返回结构化 `EvaError` |
| 文档一致性 | README、Rust 类型和 `config/schemas/` 字段命名一致 |

## 暂不实现

| 暂不实现内容 | 后续归属 |
| --- | --- |
| 完整 JSON Schema validator | `eva-config` 后续独立切片 |
| effective policy 合并 | `eva-policy` |
| routes 展开与 Topic 投递 | `eva-scheduler` |
| runtime service 装配 | `eva-runtime` |
| Lua sandbox 与 host bindings | `eva-lua-host` |
| Adapter transport 执行 | `eva-adapter` |
| CLI 命令解析和输出格式 | `eva-cli` |

## 后续顺序

| 顺序 | 模块 | 目标 |
| --- | --- | --- |
| 1 | `eva-config` | 最小配置加载和 manifest 验证 |
| 2 | `eva-cli` | 接入 `eva validate` |
| 3 | `eva-policy` | 定义 effective policy 最小类型和合并规则 |
| 4 | `eva-eventbus` | 实现 in-memory publish/subscribe |
| 5 | `eva-scheduler` | 实现 TopicPattern 路由到 Agent mailbox |
| 6 | `examples/basic/` | 串起配置加载、事件进入、路由和最小 Agent 处理路径 |

## English Summary

`eva-config` owns project configuration loading and normalization. Its first milestone is to load `eva.yaml` and Agent/Adapter/Capability manifests, validate stable fields through `eva-core`, and expose a minimal `ProjectConfig` for `eva validate` and later runtime composition.

It must not start the runtime, execute Lua, call adapters, make final permission decisions, or duplicate `eva-core` validation rules.
