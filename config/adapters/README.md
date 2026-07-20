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

旧 manifest 缺少这些区块时等价于 `restart: none` 和 `run_as: current`。自动 restart 或非 `current` identity 只允许进程型 `stdio`、Skill 和 MCP stdio；HTTP、MCP HTTP 等 transport 会在加载时拒绝。W3-L06 已为这些进程型 transport 执行有界 durable restart、backoff/jitter 和稳定窗口重置；vault 目标必须出现在同一 manifest 的 `permissions.env` 中，当前仍不会访问 vault 或注入 secret，run-as 身份切换与跨进程 admission 由后续 W3 任务负责。生产环境只接受 allowlisted `env:NAME` 敏感引用，不能把 `vault://` 直接放进 HTTP header。

新的 MCP HTTP manifest 使用 canonical `streamable_http`；`http` 仅作为 legacy alias 保留。W4-L01 已校验配置合同，但 HTTPS I/O 和证书材料读取仍由 W4-L02 实现，因此当前会在连接前 fail closed：

```yaml
transport: mcp
permissions:
  env: [GITHUB_TOKEN, MCP_CLIENT_CERT, MCP_CLIENT_KEY]
mcp:
  server_transport: streamable_http
  endpoint: https://mcp.example.com/rpc
  headers:
    Authorization: env:GITHUB_TOKEN
  http:
    trust_roots:
      - system
      - file:certs/internal-root.pem
    client_auth:
      cert_ref: env:MCP_CLIENT_CERT
      key_ref: env:MCP_CLIENT_KEY
    redirect:
      mode: same_origin
      max_hops: 3
    allowed_origins:
      - https://mcp.example.com
```

`file:` trust root 只能表示项目相对引用；W4-L02 必须相对受控项目根解析，禁止使用进程当前工作目录、绝对路径、路径穿越或内联 PEM。stdio 只能声明 `mcp.command/mcp.args`，Streamable HTTP 只能声明 `mcp.endpoint/mcp.headers/mcp.http`；未知字段和两类配置混用都会在加载时拒绝。

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

Legacy manifests default to `restart: none` and `run_as: current`. Automatic restart or a non-`current` identity is accepted only for process-backed `stdio`, Skill, and MCP stdio transports; HTTP and MCP HTTP fail during loading. W3-L06 provides bounded durable restart, backoff/jitter, and stable-window reset for those process transports; vault targets must also appear in `permissions.env`, and this task still does not contact a vault or inject secret bytes. Run-as identity switching and cross-process admission belong to later W3 tasks. Production accepts only allowlisted `env:NAME` sensitive references and rejects direct `vault://` HTTP headers.

For MCP adapters, new HTTP manifests use canonical `mcp.server_transport:
streamable_http`; `http` is retained only as a legacy alias. The W4-L01 typed
contract validates HTTP(S) endpoint/origin, trust-root and client-auth
references, same-origin redirects, allowed origins, and headers before I/O.
HTTPS I/O and certificate-material loading remain W4-L02 and currently fail
closed before connecting. The YAML example above applies to both language
sections. A `file:` trust root is a project-relative reference that W4-L02 must
resolve against the controlled project root, never process cwd; absolute paths,
path traversal, and inline PEM are forbidden. Stdio accepts only
`mcp.command/mcp.args`, while Streamable HTTP accepts only
`mcp.endpoint/mcp.headers/mcp.http`; unknown or mixed union fields are rejected
during manifest loading.
