# Project Configuration

> Language: English
> Published default: docs/en/project-configuration.md
> Translation: [简体中文](../zh-CN/项目配置方案.md)

Updated: 2026-06-16

## Purpose

This document defines the project configuration layout, manifest model, schema
validation, policy model, and hot reload limits.

## Configuration Scope

Eva-CLI configuration should cover:

- Project-level `eva.yaml`.
- Agent manifests.
- Adapter manifests.
- Capability manifests.
- Skill manifests and runtime gates, as specified in
  [Skill Implementation Plan](skill-implementation.md).
- MCP policy.
- Hardware policy.
- Sandbox policy.
- Runtime and observability settings.

## Validation Rules

Configuration is data, not executable code. Runtime must validate every file
with structured parsers and JSON Schema-like contracts before it affects
registries or execution.

## Hot Reload Boundaries

Safe hot reload can update labels, routes, subscription rules, selected policy
limits, and Lua source references. Permission expansion, transport changes,
state backend changes, MCP command changes, and sandbox changes require a
runtime generation switch or restart.

## Merge Rules

Configuration merge order must be deterministic. Project config, user config,
environment overrides, and built-in defaults should record provenance so audit
and debugging can explain why a runtime value was selected.
