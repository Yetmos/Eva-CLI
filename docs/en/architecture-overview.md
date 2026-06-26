# Architecture Overview

> Language: English
> Published default: docs/en/architecture-overview.md
> Translation: [简体中文](../zh-CN/总体架构方案.md)

Updated: 2026-06-16

## Purpose

This document is the canonical architecture entry for Eva-CLI. It consolidates
the runtime, EventBus, Adapter, Lua capability, memory, discovery, hardware,
process recovery, backup, migration package, and release snapshot designs into
one system boundary.

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
- BackupService, MigrationPackageService, ReleaseSnapshotService, and
  ArtifactStore provide trusted backup, migration, release evidence, restore,
  rollback, manifest verification, and audit boundaries.

## Architecture Diagram

![Eva-CLI architecture diagram](../../assets/eva-cli-architecture.svg)

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
- Backup, migration package, and release snapshot operations must be implemented
  as Runtime services. Agents may request and explain these operations, but
  Runtime owns scope resolution, verification, mutation, restore, rollback, and
  audit records.
- English documentation owns the default public entry and stable slugs. During
  the current migration window, mapped `docs/zh-CN/` documents remain the
  detailed implementation-spec authority.
