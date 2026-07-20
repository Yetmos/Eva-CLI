# MCP TLS test fixtures

These certificates and private keys are public, non-production fixtures used
only by the offline `eva-mcp` TLS tests. They must never be trusted or reused
outside the test suite.

- `ca.pem` signs the server and client fixture certificates.
- `server.pem` is valid for `localhost` and `127.0.0.1`.
- `wrong.pem` is valid only for `wrong.test`.
- `expired.pem` expired on 2021-01-01.
- `client.pem` is limited to client authentication.
- `unknown-ca.pem` is unrelated to the server certificate chain.

The non-expired fixtures are valid from 2026-07-20 through 2126-06-26.
