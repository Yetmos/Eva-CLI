# Adapters / 适配器配置

## 中文

Adapter manifest 声明外部能力入口、transport、capability、权限、超时、并发和路由优先级。所有 Adapter 调用必须经过 manifest、schema、policy、timeout、audit 和 structured error。

本目录不保存真实密钥。需要密钥的 Adapter 只能声明环境变量白名单，例如 `ANTHROPIC_API_KEY` 或 `GITHUB_TOKEN`。

W3-L01 增加了可选的 provider 配置：

```yaml
supervision:
  restart:
    mode: on_failure
    max_attempts: 3
    backoff_ms: 1000
  run_as:
    kind: current
credentials:
  vault:
    - env: GITHUB_TOKEN
      ref: vault://providers/github/token#value
```

旧 manifest 缺少这些区块时等价于 `restart: none` 和 `run_as: current`。自动 restart 或非 `current` identity 只允许进程型 `stdio`、Skill 和 MCP stdio；HTTP、MCP HTTP 等 transport 会在加载时拒绝。vault 目标必须出现在同一 manifest 的 `permissions.env` 中，引用会排序并进入运行时摘要，但当前任务不会访问 vault 或注入 secret；真实取密、身份切换和 restart controller 由后续 W3 任务负责。生产环境只接受 allowlisted `env:NAME` 敏感引用，不能把 `vault://` 直接放进 HTTP header。

## English

Adapter manifests declare external capability entry points, transports, capabilities, permissions, timeouts, concurrency, and routing priority. Every Adapter call must pass through manifest, schema, policy, timeout, audit, and structured error handling.

This directory never stores real secrets. Adapters that need credentials declare environment variable allowlists such as `ANTHROPIC_API_KEY` or `GITHUB_TOKEN`.

W3-L01 adds optional provider configuration:

```yaml
supervision:
  restart:
    mode: on_failure
    max_attempts: 3
    backoff_ms: 1000
  run_as:
    kind: current
credentials:
  vault:
    - env: GITHUB_TOKEN
      ref: vault://providers/github/token#value
```

Legacy manifests default to `restart: none` and `run_as: current`. Automatic restart or a non-`current` identity is accepted only for process-backed `stdio`, Skill, and MCP stdio transports; HTTP and MCP HTTP fail during loading. Vault targets must also appear in `permissions.env`; references are sorted and included in the runtime digest, but this task does not contact a vault or inject secret bytes. Real secret retrieval, identity switching, and restart control belong to later W3 tasks. Production accepts only allowlisted `env:NAME` sensitive references and rejects direct `vault://` HTTP headers.

For MCP adapters, `mcp.server_transport: stdio` continues to use `mcp.command`
and `mcp.args`. V1.13.6 also supports `mcp.server_transport: http` with
`mcp.endpoint` and optional `mcp.headers` values such as `Authorization:
env:GITHUB_TOKEN`; HTTP MCP calls inject provider session auth headers and keep
tool allowlists ahead of RPC. The built-in client is intentionally limited to
`http://` endpoints until a TLS client boundary is added.
