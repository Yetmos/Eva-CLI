# Lua External Agent Adapter

> Language: English
> Published default: docs/en/capabilities/lua-external-agent-adapter.md
> Translation: [简体中文](../../zh-CN/capabilities/Lua调用外部Agent动态Adapter架构方案.md)

Updated: 2026-06-16

## Purpose

This document defines how Lua Agents request external capabilities without
directly owning process, network, filesystem, model-provider, or hardware
access.

## Adapter Boundary

Lua calls a controlled Tool Layer. Rust resolves the call through
AdapterRegistry and AdapterRouter, then invokes the selected adapter under the
declared manifest, schema, transport, policy, timeout, and audit rules.

Supported adapter families include:

- External Agent adapters such as Claude, Codex, Gemini, and local models.
- MCP server adapters.
- Skill and workflow adapters. The concrete Skill classification, manifest,
  runtime gate, and invocation contract are defined in
  [Skill Implementation Plan](skill-implementation.md).
- CLI, HTTP, and internal service adapters.
- Hardware adapters.

## Required Manifest Fields

Each adapter needs:

- Stable adapter ID and version.
- Transport type and endpoint or command descriptor.
- Input and output schema.
- Permission policy and sandbox policy.
- Timeout, retry, concurrency, and rate-limit settings.
- Audit identity and ownership metadata.

## Security Rules

- Lua never reads provider keys or host secrets.
- Lua never executes raw shell commands.
- Runtime policy decides whether a tool call is allowed before transport work
  begins.
- Adapter output must be validated before it is returned to Lua.

## Failure Semantics

Adapter failures return structured errors with retryability, origin, audit ID,
and redaction-safe diagnostics. Lua can branch on declared error classes, but it
cannot bypass Rust policy enforcement.
