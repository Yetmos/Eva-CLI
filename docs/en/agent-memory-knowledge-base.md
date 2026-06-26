# Agent Memory and Knowledge Base

> Language: English
> Published default: docs/en/agent-memory-knowledge-base.md
> Translation: [简体中文](../zh-CN/Agent记忆与知识库架构方案.md)

Updated: 2026-06-16

## Purpose

This document defines memory ownership, knowledge retrieval, context building,
and the boundary between Lua Agents and Rust-managed state.

## Memory Layers

- Agent private memory stores scoped, Agent-owned facts and preferences.
- Global memory stores approved cross-Agent facts.
- Knowledge base stores project documents, design records, and searchable
  reference material.
- ContextBuilder assembles the allowed view for a specific Agent invocation.

## Ownership Rule

Rust is the source of truth for memory and knowledge services. Lua Agents can
read scoped context and write private memory through controlled APIs. Global
memory changes should be proposals that pass policy, audit, and optional review.

## ContextBuilder Contract

ContextBuilder must:

- Apply policy before retrieval.
- Filter by Agent identity, task scope, and data sensitivity.
- Return provenance for included facts.
- Bound token size and retrieval count.
- Record audit data for memory reads and writes.

## Consistency

EventBus can announce memory changes, but does not store memory. Durable memory
is committed through explicit stores and should support snapshots, migrations,
and rollback-safe upgrades.
