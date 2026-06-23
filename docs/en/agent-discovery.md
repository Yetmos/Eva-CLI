# Agent Discovery

> Language: English
> Canonical: docs/en/agent-discovery.md
> Translation: [简体中文](../zh-CN/Agent扫描与发现架构方案.md)

Updated: 2026-06-16

## Purpose

This document defines how Eva-CLI discovers Agents, adapters, MCP servers,
skills, Lua capabilities, and project-local configuration without turning
discovery into implicit authorization.

## Discovery Sources

- Project configuration and manifests.
- User-approved local skill directories.
- Explicit MCP server configuration.
- Adapter manifests.
- Hardware manifests and device bindings.
- Built-in runtime capabilities.

## Discovery Pipeline

1. Scan approved roots only.
2. Parse manifests with structured parsers.
3. Normalize IDs, versions, transports, schemas, and policies.
4. Validate against schemas.
5. Build a candidate registry.
6. Mark conflicts, disabled entries, and policy failures.
7. Publish only validated capabilities to runtime registries.

## Security Boundary

Discovery is not execution permission. A discovered capability still needs
manifest validation, policy approval, schema validation, sandbox selection, and
runtime audit before it can be invoked.

Skill discovery follows the same rule. A discovered `SKILL.md` becomes
display-only unless it can be classified and registered through the
[Skill Implementation Plan](skill-implementation.md) contract.

## Conflict Handling

Capability IDs must be stable. Conflicts should be deterministic and visible:
the registry records winning and rejected candidates, origin paths, versions,
and rejection reasons.
