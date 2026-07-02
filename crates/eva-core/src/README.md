# eva-core/src / 基础契约源码

![Eva module implementation roadmap](../../assets/eva-module-implementation-roadmap.svg)

本目录包含 Eva workspace 的无副作用基础契约。当前 V0.1/V0.2 已完成第一轮实现，下游模块应复用这里的 ID、Topic、Event、Invoke、Capability 和 Error 类型。

## 功能说明

| 文件 | 职责 | 当前进度 | 后续使用者 |
| --- | --- | --- | --- |
| `error.rs` | `EvaError`、`ErrorKind`、provider code、safe context | 已完成 | 全部 crate |
| `topic.rs` | `Topic`、`TopicPattern`、通配匹配 | 已完成 | Scheduler、EventBus、Config |
| `ids.rs` | Agent、Adapter、Capability、Request、Event、Generation ID | 已完成 | 全部 runtime 模块 |
| `capability.rs` | `CapabilityName`、`CapabilityRef`、`ProviderHint` | 已完成 | Capability、Adapter、MCP |
| `event.rs` | `Event`、target、payload、metadata、trace context | 已完成 | EventBus、Scheduler、Agent |
| `invoke.rs` | `InvokeRequest`、`InvokeResponse`、status、metadata | 已完成 | Agent、Capability、Adapter |
| `lib.rs` | 公共 re-export | 已完成 | 全部下游 crate |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 先定义错误和 ID，再定义 Topic 和 Capability。 | 下游可复用基础类型。 |
| 2 | 定义 Event 和 Invoke，保持 payload 不解释业务语义。 | runtime 可传递事件和调用结果。 |
| 3 | 为每个纯逻辑类型补单元测试。 | V0.1/V0.2 契约稳定。 |
| 4 | 后续只在有下游需求时扩展字段，并保持无副作用。 | 公共契约不被实现细节污染。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Error | 结构化错误 | 已完成 | 下游统一错误映射。 |
| Topic | Topic 和 pattern 匹配 | 已完成 | Scheduler 接入优先级。 |
| IDs | 稳定 ID newtype | 已完成 | 下游替换字符串 ID。 |
| Capability | capability 名称和引用 | 已完成 | CapabilityRegistry 复用。 |
| Event | 事件契约 | 已完成 | EventBus 接入。 |
| Invoke | 调用契约 | 已完成 | Adapter/Agent 接入。 |
