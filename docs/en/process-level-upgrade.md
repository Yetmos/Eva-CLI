# Process-Level Upgrade

> Language: English
> Canonical: docs/en/process-level-upgrade.md
> Translation: [简体中文](../zh-CN/进程级停机升级架构方案.md)

Updated: 2026-06-16

## Purpose

This document defines process supervision, stop/start recovery, runtime
generation switching, draining, rollback, and durable state requirements.

## Recovery Model

Eva-CLI uses layered recovery:

- OS service manager starts and restarts the Supervisor.
- Supervisor owns Runtime lifecycle.
- Runtime can report health and request controlled replacement.
- Runtime generation switching supports blue-green style activation.
- Durable Event Log and State Store preserve recoverable work.

## Upgrade Flow

1. Start a new Runtime generation.
2. Load configuration, manifests, Lua code, and adapters.
3. Run health and compatibility checks.
4. Close or drain ingress for the old generation.
5. Shift new work to the new generation.
6. Let in-flight work complete or cancel by policy.
7. Retire the old generation after audit-safe completion.

## Rollback Semantics

If activation fails, the old generation remains active. If post-activation
health fails, Supervisor can route traffic back, replay durable events when
safe, and preserve enough diagnostics to explain the failure.

## State Rules

Long-running tasks need explicit snapshot, reattach, timeout, idempotency, and
event replay contracts. Best-effort in-memory work must be marked as such.
