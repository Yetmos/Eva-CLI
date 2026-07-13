# Topic Route Source of Truth and Validation

> Language: English
> Published default: `docs/en/architecture/topic-routing-hybrid-sync.md`
> Translation: [Simplified Chinese](../../zh-CN/architecture/Topic路由混合同步方案.md)
> Translation status: current

Updated: 2026-07-13

## Purpose

This document records the implemented relationship between the configured
runtime Topic route table and Agent manifests.

**The route file resolved from `config/eva.yaml` -- normally
`config/routes/topics.yaml` -- is the only runtime source of truth for
Scheduler Topic delivery.** Agent `subscriptions` are declarations that are
parsed and displayed, but they do not generate, merge with, or replace the
route table.

Eva-CLI reads this configuration. It does not write routes during startup,
validation, Agent reload, or daemon control.

## Configuration Ownership

| Input | Implemented responsibility | Routing authority |
| --- | --- | --- |
| `config/eva.yaml` | Selects configuration roots, including `config.route_file` | Points to the route source of truth |
| `config/routes/topics.yaml` | Defines ordered `pattern`, `delivery`, and `agents` rules | Yes |
| `config/agents/**/agent.yaml` `subscriptions` | Declares Topic patterns associated with an Agent and exposes them to inspection/status surfaces | No |
| `config/agents/**/agent.yaml` `permissions.emit` | Declares Topic patterns the Agent is permitted to emit | No |
| Agent `parent` and `children` | Declare management relationships | No |

There is no generated route file or route-diff artifact in the current
contract. Agent manifests also have no typed `routes` field.

## Architecture

![Topic route source of truth and validation](../../assets/topic-routing-hybrid-sync.svg)

## Load and Validation Flow

```text
config/eva.yaml
  -> resolve ConfigRoots.route_file
  -> validate route YAML with config/schemas/routes.schema.json
  -> eva-config::load_routes -> RouteConfig
  -> load Agent manifests
  -> validate_project_config -> validate_route_agents
  -> RuntimeBuilder / basic::subscription_table
  -> eva-scheduler::SubscriptionTable
```

`load_project_config` performs schema validation before typed loading. It then
loads the route table and Agent manifests into one `ProjectConfig` and checks
cross-file target references. The basic runtime converts each `RouteRule` into
the Scheduler's corresponding `RoutingRule`.

No stage writes YAML back to disk.

## Route Schema

The implemented route shape is:

```yaml
routes:
  - pattern: /sys/route-a
    delivery: fanout
    agents:
      - agent-a
```

| Field | Implemented rule |
| --- | --- |
| `routes` | Required and must contain at least one route after typed loading |
| `pattern` | Required and parsed as a `TopicPattern` |
| `delivery` | Required; only `fanout` and `compete` are accepted |
| `agents` | Required and must contain at least one valid `AgentId` |
| Additional route fields | Rejected by the JSON Schema |

`TopicPattern` supports exact segments, `*` for one segment, and final `**` for
zero or more trailing segments. Patterns must begin with `/`, cannot contain
empty segments, and cannot end with `/`.

## Checks That Exist

`eva config validate` runs the same project loader used by runtime commands. For
routes, the implemented checks are:

- route and Agent files satisfy their JSON Schemas;
- the route table is non-empty;
- every pattern parses successfully;
- delivery is `fanout` or `compete`;
- every route contains at least one syntactically valid Agent ID; and
- every targeted Agent ID exists in the loaded Agent manifests.

Agent manifests are separately checked for unique IDs, valid parent/child
references, an existing Lua script, valid subscription and emit patterns, and
valid provider/capability references.

Target existence currently includes disabled Agent manifests. Route validation
does not require a target to have `enabled: true`.

## Checks That Do Not Exist

Current validation does not:

- require a route pattern to be covered by the target Agent's subscriptions;
- reject a subscription that has no corresponding global route;
- compare route delivery with Agent permissions;
- infer routes from `parent`, `children`, directory nesting, or subscriptions;
- detect duplicate or overlapping route patterns as conflicts;
- assign exact patterns precedence over wildcard patterns;
- validate priority, fallback, consumer groups, or load-balancing policy; or
- produce warnings, proposals, generated routes, or a route diff.

These omissions are important when reviewing configuration: a successful
`config validate` proves the implemented schema, typed, and reference checks,
not semantic subscription coverage.

## Runtime Routing Semantics

The Scheduler preserves route file order and expands every matching rule.

1. An explicit `EventTarget::Agent` bypasses Topic route expansion.
2. Otherwise all route patterns that match the Topic are processed in source
   order.
3. `fanout` delivers to every Agent listed by each matching rule.
4. `compete` currently delivers only to the first Agent listed by each matching
   rule.
5. No match returns a structured not-found error.

There is no implicit "most specific rule wins" behavior. An exact route and a
wildcard route can both produce deliveries for the same event.

## Agent Subscriptions

`subscriptions` remain useful Agent metadata:

- `eva-config` parses each value as a `TopicPattern`;
- project inspection and `eva agent status` can display the declarations.

The Scheduler does not build `SubscriptionTable` from these declarations. An
Agent can therefore declare a subscription without receiving events, and a
route can target an Agent whose declarations do not cover the route pattern.
The runtime delivery result still comes only from the route file.

## CLI Boundary

The implemented configuration command is:

```text
eva config validate [--project <path>] [--output text|json]
```

`eva inspect config` can show the loaded project configuration summary, but it
is not an effective Scheduler route dump.

The following commands do not exist:

```text
eva config routes sync --check
eva config routes sync --write
eva config routes preview
eva config routes dump-effective
```

There is no `RouteProposalBuilder`, `.eva/generated/routes/`, or
`.eva/reports/config/routes-diff.json` implementation.

## Change and Reload Boundary

Route changes are manual configuration edits:

1. Edit `config/routes/topics.yaml` or the file selected by
   `config.route_file`.
2. Run `eva config validate`.
3. Rebuild or restart the consuming runtime/command so it loads a new
   `ProjectConfig` and constructs a new `SubscriptionTable`.

Changing the YAML does not mutate an already-built table. Agent reload and
daemon reload-plan generation evidence do not reread, replace, or write the
route file. There is currently no file watcher, automatic synchronization, or
atomic live RouteTable replacement wired to this configuration.

## Decision Record

| Decision | Current result |
| --- | --- |
| Runtime route source of truth | The route file selected by `config.route_file`, normally `config/routes/topics.yaml` |
| Agent subscription role | Validated declaration and inspection/status metadata |
| Route mutation | Manual edit only |
| CLI support | Read-only `config validate`; no sync, preview, write, or effective dump command |
| Runtime update | Explicit rebuild/restart; no automatic hot replacement |
| Compete selection | First Agent listed in the matching rule |

## Summary

Eva-CLI keeps Topic delivery explicit: the route file defines who receives an
event, while Agent subscriptions describe an Agent without changing that
decision. Validation protects syntax, typed values, and target references; it
does not synthesize routes or prove subscription coverage.
