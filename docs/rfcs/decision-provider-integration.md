# Decision provider integration

## Status

Implemented as the first bounded Decision integration for the scheduler.

This RFC describes the runtime boundary for an optional model-backed
autonomous-continuation decision. It does not define a general agent planner,
shadow evaluation, rollout rules, or hot reload.

## Scope

Decision is an instance-level extension point. It may choose only one
candidate already produced by the runtime's scheduler projection. It cannot
create work, change `origin`, `trust`, `priority`, permissions, or lifecycle
state.

The static semantic hook remains the default and fallback path. With
`decision.enabled` unset or `false`, no model request is made.

## Configuration

Decision configuration is independent from the ordinary agent model route:

```yaml
decision:
  enabled: false
  route:
    endpoint: https://decision.example/v1
    model: bounded-selector
    credential_profile: decision-api
  timeout_ms: 1500
  max_tokens: 256
  concurrency: 4
  queue_capacity: 32
```

`decision.enabled: true` requires a non-empty `decision.route.endpoint` and
`decision.route.model`. Credentials are referenced by profile and are never
serialized into the Decision configuration or included in request metadata.
The Decision route is never inherited from the ordinary agent model route.

## Web GUI settings

`RuntimeConfigSurface` reports the current Decision route (`enabled`,
`endpoint`, `model`, `credential_profile`), and the Web GUI settings page
reads and writes the same four `decision.*` keys through
`/runtime/config/update`. The GUI keeps `model` as free text: a Decision route
may target an OpenAI-compatible endpoint or a JEV-specific decision model that
the shared model catalog does not list. The GUI does not enumerate or validate
remote model lists. The GUI never fills in defaults.

The runtime rejects an incomplete Decision route before persisting it:
`/runtime/config/update` validates the candidate config through the same
`decision.enabled` → `decision.route.endpoint`/`decision.route.model`
construction the runtime hook uses, so a batch that would enable the provider
without a usable route is reported as rejected with the explicit
`requires ... decision.route.endpoint/model` reason and nothing is written to
`config.json`.

Persisting such a route would otherwise be worse than a route-loading error:
`reload_config` constructs the Decision hook before the config snapshot swap,
so an invalid persisted route fails every later reload for all agents and
settings, and the next daemon restart cannot spawn the reconfigurable agent
runtime until `config.json` is edited by hand.

## Request and response contract

The runtime sends a versioned request with:

- a correlation/request id;
- a bounded scheduler snapshot;
- the static baseline candidate;
- the complete, bounded candidate set;
- schema name and schema version;
- a deadline.

The provider may return `select`, `abstain`, or a provider fallback/error. A
selected value is accepted only if it is structurally valid and exactly
matches a candidate in the current projection. The runtime then rechecks the
snapshot identity, work-item revision, generation, and reactivation mode before
applying it. Any mismatch is treated as a stale or invalid proposal and uses
the static baseline.

## Execution and failure behavior

`DecisionExecutor` is a thin asynchronous boundary:

- scheduler work is not blocked by a synchronous provider call;
- a bounded semaphore limits in-flight requests;
- a pending-request limit bounds queue admission;
- deadline expiry cancels the provider context;
- provider, schema, stale-snapshot, invalid-proposal, and queue failures
  resolve to the static baseline;
- results are never applied after the scheduler snapshot is stale.

There is no automatic route fallback to the ordinary agent model and no
automatic retry in this first version.

## Observability and trust boundary

Fallback events record only sanitized runtime facts such as the agent id,
fallback reason, and candidate count. Request metadata contains integration
and policy labels, not prompt contents, credentials, or provider secrets.
Provider response content is validated and is not copied into user-facing
briefs or arbitrary runtime commands.

Future work may add metrics or sampled traces, but must preserve this
metadata-only boundary.
