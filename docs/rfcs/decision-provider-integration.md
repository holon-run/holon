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

Decision reuses an existing turn-capable model route. Endpoint, transport, model
catalog entry, and credentials are configured once under the ordinary provider
configuration:

```yaml
decision:
  enabled: false
  model: openai/gpt-4o-mini
  timeout_ms: 1500
  max_tokens: 256
  concurrency: 4
  queue_capacity: 32
```

`decision.enabled: true` requires a non-empty `decision.model` that resolves to
an existing turn-capable model route. OpenAI-compatible transports use the
OpenAI adapter; Jev-compatible transports use the built-in typed Jev adapter.
Credentials remain owned by the provider configuration and are never serialized
into the Decision configuration or request metadata. The old `decision.route.*`
configuration is intentionally not migrated.

## Web GUI settings

`RuntimeConfigSurface` reports `enabled`, the selected `model` reference, and
the independent `local_onnx` settings. The Web GUI reads and writes these
`decision.*` keys through `/runtime/config/update`; it does not duplicate
provider endpoints or credentials.

The runtime rejects an unresolved Decision model before persisting it:
`/runtime/config/update` validates the candidate config through the same
`decision.enabled` → shared model catalog resolution used by the runtime hook,
so a batch that would enable the provider without a usable model reference is
reported as rejected and nothing is written to `config.json`.

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
