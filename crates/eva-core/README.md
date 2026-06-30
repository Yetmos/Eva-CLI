# eva-core / 基础契约

![eva-core contract boundary](../../docs/assets/eva-core-contract-boundary.svg)

## 中文

`eva-core` 是 Eva-CLI 的基础契约 crate，负责保存跨模块共享、长期稳定、无副作用的数据类型。它是上层运行时、配置、事件总线、调度器、Agent、Adapter、MCP、存储和 CLI 的共同语言。

这个 crate 的目标不是“做事情”，而是定义“系统里被传递的东西长什么样”。因此它应该保持轻量、纯粹、稳定，避免直接依赖 Tokio 任务装配、文件系统、网络、Lua、MCP、数据库或具体 provider。

### 职责边界

| 范围 | 负责 | 不负责 |
| --- | --- | --- |
| 事件契约 | `Event`、事件标识、Topic、target、payload、correlation/causation 字段 | 事件持久化、广播、恢复、死信处理 |
| Topic 契约 | Topic 名称、Topic pattern、匹配安全约束 | 订阅表、投递策略、Agent mailbox |
| 标识符 | `AgentId`、`AdapterId`、`CapabilityName`、`RequestId`、`EventId` 等稳定 ID | ID 存储、注册表生命周期 |
| 调用契约 | Agent/Capability/Adapter invoke request 与 response 的稳定形态 | 实际调用 Lua、HTTP、stdio、MCP 或硬件 |
| 错误契约 | 跨 crate 传播的结构化错误类型和错误分类 | 日志输出、审计落盘、重试调度 |

### 模块说明

| 模块 | 计划承载的契约 | 下游使用者 |
| --- | --- | --- |
| `event` | 运行时事件、payload、target、时间和链路追踪字段 | `eva-eventbus`、`eva-scheduler`、`eva-agent` |
| `topic` | Topic 名称、pattern、通配规则和校验结果 | `eva-scheduler`、`eva-policy`、`eva-config` |
| `ids` | Agent、Adapter、Capability、Request、Event 等 ID newtype | 几乎所有 crate |
| `capability` | Capability 名称、provider 选择所需的基础类型 | `eva-capability`、`eva-adapter`、`eva-agent` |
| `invoke` | Agent invoke、Capability invoke、响应和状态枚举 | `eva-agent`、`eva-runtime`、`eva-cli` |
| `error` | `EvaError`、`ErrorKind`、retryable 和 provider code | 所有跨模块调用边界 |

### 依赖规则

| 规则 | 原因 |
| --- | --- |
| `eva-core` 不依赖其他 Eva runtime crate | 避免形成循环依赖，让它成为稳定底座 |
| 不直接访问文件、网络、数据库、shell 或硬件 | 保持契约层无副作用，便于测试和复用 |
| 不持有全局可变状态 | 防止基础类型隐藏运行时行为 |
| 不包含 provider 私有协议 | provider 细节属于 `eva-adapter` 或具体 transport |
| 对外类型优先使用显式 newtype | 防止把 Agent ID、Adapter ID、Topic 字符串混用 |

### 第一阶段建议

| 优先级 | 契约 | 最小交付 |
| --- | --- | --- |
| 1 | Topic | `Topic`、`TopicPattern`、基础校验和匹配错误 |
| 2 | ID | `AgentId`、`AdapterId`、`CapabilityName`、`RequestId`、`EventId` |
| 3 | Event | 事件主体、target、payload、链路追踪字段 |
| 4 | Invoke | Agent/Capability 调用请求和响应 |
| 5 | Error | 结构化错误分类、retryable、provider code |

### 与其他 crate 的关系

```text
eva-core
  -> 被 eva-config 用来构造强类型配置引用
  -> 被 eva-eventbus 用来发布和恢复 Event
  -> 被 eva-scheduler 用来匹配 Topic 并投递目标
  -> 被 eva-agent 用来定义 Agent 事件处理边界
  -> 被 eva-adapter / eva-capability 用来定义能力调用契约
  -> 被 eva-runtime / eva-cli 用来组合和暴露稳定用户入口
```

## English

`eva-core` is the foundational contract crate for Eva-CLI. It owns shared, long-lived, side-effect-free data types used across runtime, configuration, event bus, scheduler, Agent, Adapter, MCP, storage, and CLI crates.

This crate should not “do work”; it should define the shapes of the things that move through the system. Keep it small, pure, and stable. It should not directly depend on Tokio task wiring, filesystem access, networking, Lua, MCP, databases, or provider-specific logic.

### Responsibility Boundary

| Area | Owns | Does Not Own |
| --- | --- | --- |
| Event contracts | `Event`, event ids, Topic, target, payload, correlation/causation fields | Event persistence, broadcasting, recovery, dead-letter handling |
| Topic contracts | Topic names, Topic patterns, matching-safe constraints | Subscription tables, delivery policy, Agent mailboxes |
| Identifiers | Stable newtypes such as `AgentId`, `AdapterId`, `CapabilityName`, `RequestId`, `EventId` | ID storage or registry lifecycle |
| Invoke contracts | Stable Agent/Capability/Adapter request and response shapes | Calling Lua, HTTP, stdio, MCP, or hardware |
| Error contracts | Structured cross-crate errors and error categories | Log output, audit persistence, retry scheduling |

### Module Map

| Module | Planned Contract | Downstream Users |
| --- | --- | --- |
| `event` | Runtime events, payload, target, timestamps, trace linkage | `eva-eventbus`, `eva-scheduler`, `eva-agent` |
| `topic` | Topic names, patterns, wildcard rules, validation results | `eva-scheduler`, `eva-policy`, `eva-config` |
| `ids` | Newtypes for Agent, Adapter, Capability, Request, Event ids | Almost every crate |
| `capability` | Capability names and provider-selection primitives | `eva-capability`, `eva-adapter`, `eva-agent` |
| `invoke` | Agent invoke, Capability invoke, responses, status enums | `eva-agent`, `eva-runtime`, `eva-cli` |
| `error` | `EvaError`, `ErrorKind`, retryable flags, provider codes | All cross-module boundaries |

### Dependency Rules

| Rule | Reason |
| --- | --- |
| `eva-core` must not depend on other Eva runtime crates | Keeps it as the stable base and avoids dependency cycles |
| Do not access files, network, databases, shell, or hardware directly | Keeps contracts side-effect-free and easy to test |
| Do not hold global mutable state | Prevents foundational types from hiding runtime behavior |
| Do not include provider-private protocol details | Provider details belong in `eva-adapter` or a transport module |
| Prefer explicit newtypes for public contracts | Prevents mixing Agent ids, Adapter ids, and Topic strings |

### First Milestone

| Priority | Contract | Minimum Deliverable |
| --- | --- | --- |
| 1 | Topic | `Topic`, `TopicPattern`, basic validation, and match errors |
| 2 | ID | `AgentId`, `AdapterId`, `CapabilityName`, `RequestId`, `EventId` |
| 3 | Event | Event body, target, payload, and trace linkage fields |
| 4 | Invoke | Agent/Capability request and response contracts |
| 5 | Error | Structured error categories, retryable flag, provider code |

### Relationship With Other Crates

```text
eva-core
  -> used by eva-config for strongly typed configuration references
  -> used by eva-eventbus to publish and recover Events
  -> used by eva-scheduler to match Topics and deliver targets
  -> used by eva-agent to define the Agent event handling boundary
  -> used by eva-adapter / eva-capability to define capability invocation contracts
  -> used by eva-runtime / eva-cli to compose and expose stable user entry points
```
