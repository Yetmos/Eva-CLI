# Architecture Overview

> Language: English
> Canonical: docs/en/architecture-overview.md
> Translation: [简体中文](../zh-CN/总体架构方案.md)

Updated: 2026-06-16

## Purpose

This document is the canonical architecture entry for Eva-CLI. It consolidates
the runtime, EventBus, Adapter, Lua capability, memory, discovery, hardware, and
process recovery designs into one system boundary.

## Architecture Decision

Eva-CLI is a Rust-managed, Topic EventBus driven multi-Agent runtime. Agent
business behavior is implemented in Lua so workflows can be updated without
rebuilding the host process, while Rust keeps authority over permissions,
schemas, sandboxing, external I/O, recovery, audit, and long-term state.

The system deliberately does not clone a centralized LangGraph-style state
machine. Eva-CLI models the local runtime boundary around Agents, capabilities,
events, memory, and external adapters.

## Runtime Layers

- Ingress accepts CLI, API, UI, and external events.
- Recoverable EventBus routes typed Topic events.
- Scheduler delivers events to Agent private queues.
- Lua Agent Runtime executes local business logic in isolated states.
- AdapterRegistry exposes external Agents, MCP servers, CLI tools, HTTP APIs,
  local models, skills, and hardware capabilities.
- MemoryService, KnowledgeService, and ContextBuilder provide controlled context.
- Supervisor and Runtime generation switching provide process recovery and
  upgrade safety.

## Non-Goals

- Lua must not directly execute shell commands, read secrets, scan user
  directories, or open arbitrary network connections.
- EventBus must not become a hidden global business state store.
- Discovery must not register arbitrary executables without explicit manifests,
  schemas, and policies.
- MCP must not become an unrestricted proxy into the host environment.

## Implementation Contracts

- Every executable capability needs a manifest, schema, policy, owner, version,
  and audit identity.
- Hot reload can update scripts, routes, registrations, and selected manifest
  fields. Permission expansion, transport changes, state backend changes, and
  MCP command changes require a stricter runtime switch or restart path.
- English canonical documentation is the source for future implementation
  specs; translations must not change architectural conclusions.
