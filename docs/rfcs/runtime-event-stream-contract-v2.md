# Runtime Event Stream Contract v2

## Status

Accepted. The public envelope is version 3; Web and Android clients are released
together with the server and do not retain a pre-v3 compatibility window.

## Context

Holon's durable audit feed historically used a string `kind` and arbitrary JSON
payload. The per-agent `event_seq` was durable and ordered, but a client could
not distinguish a replaced event log from a normal daemon restart, determine
which payload schema applied, or safely detect conflicting content for the same
identity.

This RFC defines the minimum typed contract required by first-party clients. It
does not require every historical producer to migrate in one change.

## Identity and immutability

The canonical stream identity is:

```text
(event_log_epoch, agent_id, event_seq)
```

`event_log_epoch` is stored in the runtime database. Reopening the same database
preserves it. Replacing or rebuilding the database creates a new epoch.

Content for one canonical identity is immutable. A repeated append/import with
identical identity and content is idempotent. Different content is a contract
error; storage must not silently apply last-write-wins.

The event UUID remains a stable evidence reference, but it is not a replay
cursor and does not replace the canonical stream identity.

## Envelope

Every event envelope exposes:

- `event_log_epoch`
- `agent_id`
- `event_seq`
- `type`
- timestamp, event id, and payload

Typed events additionally expose `payload_schema` and
`payload_schema_version`. Legacy events are schema-less and carry their raw
payload without repeated contract metadata.

The event contract version is declared once per transport:

- event pages expose top-level `contract_version` and the
  `x-holon-event-contract-version` response header;
- SSE streams expose `x-holon-event-contract-version`.

The page response also exposes `event_log_epoch` at the top level so an empty
page can still invalidate a cached cursor and projection from a replaced log.

The envelope contract version evolves independently from payload schemas.
Payload versions evolve per schema so changing one event family does not
invalidate unrelated cached families.

The global `/events/stream` endpoint is a discovery stream, not an event
replay stream. It emits `agent_roster_hint` SSE records whose data contains
only `{ "agent_id": "..." }`; these records have no cursor and carry no audit
payload. Clients refresh the authoritative roster and consume recoverable
events only from `/agents/{agent_id}/events/stream`.

## Registry and typed payloads

`RuntimeEventKind` and its registry descriptor are the source of truth for
typed event names, payload schema ids, payload versions, display family, and
checked JSON fixtures.

A typed producer must construct an event through `AuditEvent::typed`. This binds
the Rust payload type to one registered schema descriptor. The first migration
slice covers message lifecycle, brief creation, task lifecycle, WorkItem
lifecycle, and agent-state events.

Events not yet migrated must use the explicitly named `AuditEvent::legacy`
boundary. Legacy records deserialize with contract version `1` and the
`holon.runtime_event.legacy` payload schema.

### Public kind matrix

The server-side registry and `PUBLIC_TYPED_RUNTIME_EVENT_WIRE_NAMES` define
the stable typed portion of the public event matrix:

| Kind | Family | Payload schema | Projection effect |
| --- | --- | --- | --- |
| `message_enqueued` | message | `holon.runtime_event.message_lifecycle` | `display_invalidation` |
| `message_processing_started` | message | `holon.runtime_event.message_lifecycle` | `display_invalidation` |
| `brief_created` | brief | `holon.runtime_event.brief_created` | `display_invalidation` |
| `task_created` | task | `holon.runtime_event.task_lifecycle` | `display_invalidation` |
| `task_status_updated` | task | `holon.runtime_event.task_lifecycle` | `display_invalidation` |
| `task_result_received` | task | `holon.runtime_event.task_lifecycle` | `display_invalidation` |
| `work_item_written` | work item | `holon.runtime_event.work_item_lifecycle` | `display_invalidation` |
| `agent_state_changed` | agent state | `holon.runtime_event.agent_state_changed` | `display_invalidation` |
| `scheduler_diagnostic` | scheduler | `holon.runtime_event.scheduler_diagnostic` | `none` |

All other kinds remain opaque `legacy` events unless and until they are added
to this matrix through a typed producer and registry descriptor. This includes
diagnostic, recovery, bootstrap, deletion, provider, tool, and turn records
that may still be present in the durable event stream. They are retained in
event pages and SSE so replay cursors remain lossless; clients may display
them diagnostically but must not infer a domain schema from their names or
payloads. `internal` describes their runtime role, not a second wire format
or a client-side allowlist.

The Web client therefore does not duplicate this matrix. It accepts any
non-empty event type, uses the server-provided `projection_effect` when
present, and preserves unknown legacy records without applying them as typed
domain transitions.

## Compatibility and unknown events

Clients must:

1. reject a stream or page whose declared contract version is not the current
   version;
2. preserve unknown kind, schema, version, and opaque payload;
3. avoid applying unknown payloads as known domain transitions;
4. invalidate cached projection state when `event_log_epoch` changes;
5. invalidate and bootstrap again if one canonical identity has conflicting
   immutable content.

Unknown events remain visible for diagnostics. They are not dropped and do not
crash the stream.

## HTTP, OpenAPI, and generated TypeScript

Event pages and SSE use the same Rust `StreamEventEnvelope` constructor. OpenAPI
publishes this envelope and the page response as concrete schemas. Web transport
code aliases the generated TypeScript schemas and performs compatibility
decoding only at the transport boundary.

The UI projection and presentation policy remain separate from the wire
registry.

Public event payloads retain usage and presentation fields such as token
counts, durations, and model selection. Provider-internal diagnostics
(`prompt_cache_key`, `context_fingerprint`, `compression_epoch`, provider
request/message ids, request diagnostics, attempt timelines, and
`only_sleep_tools`) remain in the durable audit/transcript records but are
removed from the default event-page and agent-SSE payload.

## Rollout

1. Add epoch persistence, registry, typed constructor, and an explicit legacy
   constructor.
2. Migrate the primary event families and add registry/fixture tests.
3. Move first-party clients to generated envelope types and explicit unknown
   handling.
4. Migrate remaining legacy producers incrementally.
5. Remove per-event contract/provenance duplication and publish the stream-level
   contract declaration.

## Non-goals

- Migrating every audit event producer in this change.
- Defining UI labels, display priority, or timeline item schemas.
- Replacing the raw event stream with a server-side UI projection.
- Introducing a second JSON Schema generation framework beside Rust/OpenAPI.
