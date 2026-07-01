# eva-core/src

This directory contains side-effect-free data contracts shared across Eva
modules.

Implemented contracts:

- `error`: `EvaError`, `ErrorKind`, provider codes, and non-sensitive context.
- `topic`: concrete `Topic`, `TopicPattern`, `*` and trailing `**` matching.
- `ids`: strong ID newtypes for Agents, Adapters, Capabilities, Requests,
  Events, and Generations.
- `capability`: dot-separated `CapabilityName`, provider hints, and
  `CapabilityRef`.
- `event`: `Event`, targets, opaque payloads, metadata, and trace context.
- `invoke`: invoke targets, requests, responses, statuses, and metadata.

Keep this crate free of runtime wiring, file access, network access, shell
execution, databases, Lua, MCP, hardware I/O, and provider-private protocols.
