> Language: 简体中文
> English default entry: ../en/eva-core-module.md
> Translation status: current

# eva-core 模块设计

更新日期：2026-06-30

`eva-core` 是 Eva-CLI Rust workspace 的基础契约模块。它负责定义跨模块共享的稳定数据结构，让 EventBus、Scheduler、AgentRuntime、Adapter、Capability、Runtime 和 CLI 使用同一套事件、Topic、ID、调用请求和错误模型。

![eva-core 基础契约边界](../../assets/eva-core-contract-boundary.svg)

## 1. 模块定位

`eva-core` 的职责是定义系统共同语言，而不是执行系统副作用。它应该保持无 I/O、无 runtime 任务装配、无 provider 私有协议、无隐式全局状态。

| 设计目标 | 说明 |
| --- | --- |
| 稳定契约 | 把跨 crate API 中会长期传播的数据类型固定下来。 |
| 低耦合 | 下游模块依赖 `eva-core`，但 `eva-core` 不反向依赖运行时模块。 |
| 无副作用 | 不直接访问文件、网络、数据库、shell、Lua、MCP 或硬件。 |
| 强类型边界 | 用 newtype 区分 Agent ID、Adapter ID、Capability 名称、Topic 和 Request ID。 |
| 可测试 | 基础类型的校验、解析和匹配逻辑可以用纯单元测试覆盖。 |

## 2. 职责边界

| 范围 | `eva-core` 负责 | 不负责 |
| --- | --- | --- |
| Event | 事件主体、Topic、target、payload、时间戳、链路追踪字段 | 事件广播、持久化、重放、死信存储 |
| Topic | Topic 名称、Topic pattern、通配规则、校验错误 | 订阅注册表、投递策略、Agent mailbox |
| ID | `AgentId`、`AdapterId`、`CapabilityName`、`RequestId`、`EventId` 等稳定 ID 类型 | ID 分配策略、注册表生命周期 |
| Invoke | Agent、Capability、Adapter 调用请求与响应契约 | 实际执行 Lua、HTTP、stdio、MCP、硬件 |
| Error | 跨 crate 传播的结构化错误和错误分类 | 日志落盘、审计输出、重试调度 |

## 3. 子模块规划

| 子模块 | 计划内容 | 主要下游 |
| --- | --- | --- |
| `ids` | 稳定 ID newtype、解析、显示、序列化 | 全部 runtime crate |
| `topic` | `Topic`、`TopicPattern`、通配匹配、格式校验 | `eva-scheduler`、`eva-policy`、`eva-config` |
| `event` | `Event`、`EventTarget`、payload、correlation/causation 字段 | `eva-eventbus`、`eva-scheduler`、`eva-agent` |
| `capability` | `CapabilityName`、provider 选择所需基础类型 | `eva-capability`、`eva-adapter` |
| `invoke` | Agent/Capability 调用 request、response、状态枚举 | `eva-agent`、`eva-runtime`、`eva-cli` |
| `error` | `EvaError`、`ErrorKind`、retryable、provider code | 所有跨模块调用边界 |

## 4. 推荐最小交付

第一阶段不需要一次性实现所有业务对象，应先锁定会影响下游模块的最小契约。

| 优先级 | 契约 | 验收标准 |
| --- | --- | --- |
| 1 | Topic | 能解析 `/input/user`、`/sys/route-a`，拒绝空段和非法前缀。 |
| 2 | TopicPattern | 支持 exact、`*`、`**`，并保证 `**` 只能出现在最后一段。 |
| 3 | ID newtype | `AgentId`、`AdapterId`、`RequestId` 等类型不能互相误用。 |
| 4 | Event | 事件必须携带 `event_id`、`topic`、payload 和可选链路字段。 |
| 5 | Error | 错误必须包含 kind、message、retryable 和可选 provider code。 |

## 5. 依赖规则

```text
eva-core
  <- eva-config
  <- eva-policy
  <- eva-eventbus
  <- eva-scheduler
  <- eva-agent
  <- eva-capability
  <- eva-adapter
  <- eva-runtime
  <- eva-cli
```

约束：

- `eva-core` 不依赖其他 Eva runtime crate。
- `eva-core` 可以依赖小而稳定的通用库，但新增依赖必须服务于契约表达，例如序列化、时间或错误派生。
- 不在 `eva-core` 中实现 provider、transport、registry、runtime builder 或 CLI 命令。
- 不在 `eva-core` 中读取 `config/`，只定义配置加载后可能引用的基础类型。

## 6. 与运行时链路的关系

| 运行时节点 | 使用 `eva-core` 的方式 |
| --- | --- |
| Ingress | 构造合法 `Event` 或 invoke request。 |
| EventBus | 发布、记录和恢复 `Event`。 |
| Scheduler | 使用 `Topic` / `TopicPattern` 选择目标 Agent。 |
| AgentRuntime | 把事件交给 Lua，并接收结构化响应或错误。 |
| CapabilityRouter | 使用 `CapabilityName` 和 invoke request 选择 provider。 |
| AdapterRuntime | 返回统一 response 或 `EvaError`。 |
| CLI | 输出稳定的 inspect、emit、validate 结果。 |

## 7. 不应提前放入的内容

| 不应放入 | 应归属模块 |
| --- | --- |
| YAML/JSON Schema 加载 | `eva-config` |
| 权限合并与 effective policy | `eva-policy` |
| 事件日志和死信队列 | `eva-eventbus` / `eva-storage` |
| Lua State、沙箱和 binding | `eva-lua-host` |
| Adapter manifest、transport runtime | `eva-adapter` |
| MCP server/client 协议细节 | `eva-mcp` |
| Runtime generation、drain、rollback | `eva-lifecycle` / `eva-runtime` |

## 8. 总结

`eva-core` 应该优先实现 Topic、ID、Event、Invoke 和 Error 五类基础契约。它越稳定，后续 `eva-config`、`eva-eventbus`、`eva-scheduler`、`eva-agent` 和 `eva-adapter` 的实现越少返工。

