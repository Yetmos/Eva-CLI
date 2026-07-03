# eva-adapter/src / Adapter 源码

![V1.x extension module flow](../../assets/eva-extension-module-flow.svg)

## V1.1 Implemented Surface

- `manifest.rs`: defines `AdapterHandle`, `AdapterHealth`, and `AdapterCapabilityBinding`.
- `registry.rs`: indexes handles by Adapter id and capability.
- `router.rs`: supports explicit provider routing and capability-index fallback.
- `runtime.rs`: exposes `AdapterRuntime::from_project`, `list`, `probe_adapter`, `probe_capability`, and `invoke`.
- `transports/builtin.rs`: returns local controlled envelopes for builtin-style internal transports.
- `transports/mcp.rs`: enforces MCP tool allowlists through `eva-mcp` before returning an invocation envelope.
- `transports/skill.rs`: validates the Skill runtime gate and returns an audit-bearing controlled envelope.

V1.1 does not launch stdio/http/hardware providers. That is deliberate so external execution can be added later with policy, audit, credential, timeout, and platform controls in place.

本目录承载 Adapter runtime descriptor、registry、router、transport runtime 和错误映射。当前为骨架，V1.1 先实现 registry/router 和 builtin/stdio/http/MCP/skill transport。

## 功能说明

| 文件/目录 | 职责 | 当前进度 | 目标版本 |
| --- | --- | --- | --- |
| `lib.rs` | 模块导出 | 骨架 | V1.1 |
| `manifest.rs` | Adapter manifest 的 runtime 表示 | 骨架 | V1.1 |
| `registry.rs` | Adapter handle 和 capability index | 骨架 | V1.1 |
| `router.rs` | explicit provider 和 capability 路由 | 骨架 | V1.1 |
| `runtime.rs` | 授权后 transport 执行、timeout、audit | 骨架 | V1.1 |
| `error.rs` | provider/transport 错误映射 | 骨架 | V1.1 |
| `transports/` | 具体 transport 实现 | 骨架 | V1.1/V1.3 |

## 开发实施步骤

| 顺序 | 步骤 | 输出 |
| --- | --- | --- |
| 1 | 定义 runtime descriptor 和 registry。 | Adapter 可注册和查询。 |
| 2 | 实现 router 和 policy gate。 | provider 选择可解释。 |
| 3 | 实现 invocation envelope、timeout、audit、error mapping。 | 外部调用错误稳定。 |
| 4 | 分批实现 builtin、stdio、http、mcp、skill、hardware transport。 | 外部能力受控执行。 |

## 进度表

| 模块 | 具体功能 | 状态 | 下一步 |
| --- | --- | --- | --- |
| Manifest | runtime descriptor | 未实现 | 从 AdapterManifest 转换。 |
| Registry | ID 和 capability index | 未实现 | 处理重复和禁用。 |
| Router | provider selection | 未实现 | explicit 优先。 |
| Runtime | execution envelope | 未实现 | 加入 timeout 和 audit。 |
| Transport | 具体调用 | 未实现 | 先 builtin/stdio/http。 |
