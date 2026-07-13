# Crates / Rust 模块

`crates/` 承载 Eva-CLI workspace 的 Rust 模块边界。公共契约由对应 crate 单独拥有，跨模块副作用由 `eva-runtime` 组合，`eva-cli` 只负责命令输入与输出。

## Ownership 规则

- 公共类型、配置、策略和观测契约分别由 `eva-core`、`eva-config`、`eva-policy` 和 `eva-observability` 维护。
- 存储、事件、调度、Agent、Lua 和 capability 各自保持单一职责；需要跨模块协作时由 runtime 组合。
- Adapter、MCP、Discovery、Memory、Hardware、Backup 和 Lifecycle 的外部或高风险操作必须经过 manifest、policy、audit 与结构化错误边界。
- crate 根 README 记录公开职责、支持范围和验证入口；实现细节以源码、测试和 rustdoc 为准。

## 模块索引

| 模块 | Owner 职责 | README |
| --- | --- | --- |
| `eva-core` | Event、Topic、Invoke、ID、Capability 和错误等基础契约。 | [README](eva-core/README.md) |
| `eva-config` | `eva.yaml`、manifest、policy、routes 和 schema 的加载与一致性校验。 | [README](eva-config/README.md) |
| `eva-policy` | 权限集合、策略域、effective policy 和高风险操作门禁。 | [README](eva-policy/README.md) |
| `eva-observability` | Audit、metrics、tracing、exporter 与 retention 契约。 | [README](eva-observability/README.md) |
| `eva-storage` | State、event log、task、audit、artifact 和 provider process 持久化边界。 | [README](eva-storage/README.md) |
| `eva-eventbus` | Event 发布、ack/fail、dead letter 和 replay/redrive。 | [README](eva-eventbus/README.md) |
| `eva-scheduler` | Topic 路由、订阅、mailbox 投递和 retry dispatch。 | [README](eva-scheduler/README.md) |
| `eva-agent` | Agent 队列、生命周期、timeout、cancel、retry 和 drain/reload 控制。 | [README](eva-agent/README.md) |
| `eva-lua-host` | 受限 Lua VM、host binding、资源限制和 generation 生命周期。 | [README](eva-lua-host/README.md) |
| `eva-capability` | Capability registry、provider plan、router 和 host 调用边界。 | [README](eva-capability/README.md) |
| `eva-adapter` | 外部 provider manifest、registry、transport、授权与监督边界。 | [README](eva-adapter/README.md) |
| `eva-mcp` | MCP JSON-RPC、session、tool mapping、policy helper 和受控 server surface。 | [README](eva-mcp/README.md) |
| `eva-discovery` | 可信来源发现、候选归一化、健康状态和缓存。 | [README](eva-discovery/README.md) |
| `eva-memory` | 私有/全局记忆、知识检索、context 构建、脱敏和维护。 | [README](eva-memory/README.md) |
| `eva-hardware` | 设备发现、身份、driver、lease、hotplug 和 raw-I/O 安全边界。 | [README](eva-hardware/README.md) |
| `eva-backup` | Backup archive、manifest、snapshot、restore plan 和校验。 | [README](eva-backup/README.md) |
| `eva-lifecycle` | Supervisor generation、drain、handoff、rollback 和 service-manager 抽象。 | [README](eva-lifecycle/README.md) |
| `eva-release` | Release readiness、安全、性能、迁移和兼容性门禁。 | [README](eva-release/README.md) |
| `eva-runtime` | Composition root、basic loop、daemon control、recovery 和运行时观测装配。 | [README](eva-runtime/README.md) |
| `eva-cli` | 命令解析、文本/JSON 契约、trace、exit code 和操作入口。 | [README](eva-cli/README.md) |

## 验证

针对单模块修改，先运行 `cargo test -p <crate>`。合入前的 workspace 验证：

```powershell
cargo fmt --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
