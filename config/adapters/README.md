# Adapters / 适配器配置

## 中文

Adapter manifest 声明外部能力入口、transport、capability、权限、超时、并发和路由优先级。所有 Adapter 调用必须经过 manifest、schema、policy、timeout、audit 和 structured error。

本目录不保存真实密钥。需要密钥的 Adapter 只能声明环境变量白名单，例如 `ANTHROPIC_API_KEY` 或 `GITHUB_TOKEN`。

## English

Adapter manifests declare external capability entry points, transports, capabilities, permissions, timeouts, concurrency, and routing priority. Every Adapter call must pass through manifest, schema, policy, timeout, audit, and structured error handling.

This directory never stores real secrets. Adapters that need credentials declare environment variable allowlists such as `ANTHROPIC_API_KEY` or `GITHUB_TOKEN`.

For MCP adapters, `mcp.server_transport: stdio` continues to use `mcp.command`
and `mcp.args`. V1.13.6 also supports `mcp.server_transport: http` with
`mcp.endpoint` and optional `mcp.headers` values such as `Authorization:
env:GITHUB_TOKEN`; HTTP MCP calls inject provider session auth headers and keep
tool allowlists ahead of RPC. The built-in client is intentionally limited to
`http://` endpoints until a TLS client boundary is added.
