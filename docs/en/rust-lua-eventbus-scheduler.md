# Rust, Lua, and EventBus Scheduler

> Language: English
> Canonical: docs/en/rust-lua-eventbus-scheduler.md
> Translation: [简体中文](../zh-CN/Rust与Lua事件总线智能体调度架构方案.md)

Updated: 2026-06-16

## Purpose

This document defines the core Agent scheduling model: Rust hosts the runtime,
Lua implements Agent behavior, and Topic EventBus carries events between
ingress, Scheduler, Agents, and adapters.

## Core Model

- Rust owns the async runtime, EventBus, Scheduler, Agent lifecycle, isolation,
  timeout, retry, metrics, and recovery.
- Lua owns intent recognition, local state transitions, tool orchestration, and
  result mapping.
- Topic names are route addresses such as `/input/user`, `/task/created`, and
  `/sys/route-a/route-aa`.
- Scheduler subscribes to Topic patterns and delivers events to Agent private
  queues according to target, priority, load, and policy.
- Each Agent owns an isolated Lua state and private queue.

## EventBus Modes

Eva-CLI supports three deployment shapes:

- In-process best-effort EventBus for simple local execution.
- Recoverable in-process EventBus with durable event log and snapshots.
- External durable queue integration for distributed or long-running workloads.

The EventBus carries coordination events. Durable business state belongs to
explicit state stores and memory services, not to implicit EventBus internals.

## Scheduler Rules

- Match by Topic, target Agent, subscription rules, priority, and policy.
- Preserve per-Agent queue isolation.
- Apply backpressure and timeout handling at runtime boundaries.
- Record trace IDs, causality, retries, and audit fields for observability.

## Hot Reload Rules

Lua Agent script updates use generation switching. A new generation is loaded,
validated, and routed only after it passes manifest and sandbox checks. If
validation or activation fails, the previous generation remains active.
