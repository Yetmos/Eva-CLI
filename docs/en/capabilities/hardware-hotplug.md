# Hardware Hotplug

> Language: English
> Published default: docs/en/capabilities/hardware-hotplug.md
> Translation: [简体中文](../../zh-CN/capabilities/外接硬件接入与热插拔架构方案.md)

Updated: 2026-06-16

## Purpose

This document defines how Eva-CLI integrates USB, serial, BLE, network devices,
and vendor SDK devices through controlled hardware adapters.

## Hardware Boundary

Lua never accesses raw device handles, system device paths, or unchecked I/O.
Rust owns device discovery, driver binding, handle lifecycle, permissions,
event subscription, and reconnection behavior.

## Core Components

- HardwareDiscoveryService scans approved device classes.
- DeviceRegistry stores logical devices and physical bindings.
- DriverBinding selects a driver or vendor SDK adapter.
- HardwareAdapterRuntime exposes controlled capabilities to Agents.
- Hardware EventBridge emits hotplug and telemetry events to EventBus.

## Hotplug Semantics

- Device connect, disconnect, reconnect, and permission changes produce typed
  events.
- Logical device identity should survive physical path changes when possible.
- In-flight calls must return structured errors when a device disappears.
- Reconnect should not silently widen permissions.

## Policy Requirements

Hardware manifests must declare device matching rules, allowed operations,
transport type, raw I/O policy, rate limits, audit fields, and whether the
device can be used by Lua-facing capabilities.
