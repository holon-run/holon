# Runtime Event Stream Contract v2

## Status

Accepted for incremental implementation.

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

Every page and SSE event exposes:

- `event_log_epoch`
- `agent_id`
- `event_seq`
- `contract_version`
- `type`
- `payload_schema`
- `payload_schema_version`
- timestamp, event id, provenance, and payload

The page response also exposes `event_log_epoch` at the top level so an empty
page can still invalidate a cached cursor and projection from a replaced log.

The envelope contract version evolves independently from payload schemas.
Payload versions evolve per schema so changing one event family does not
invalidate unrelated cached families.

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

## Compatibility and unknown events

Clients must:

1. accept legacy envelopes that omit v2 metadata;
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

## Rollout

1. Add v2 metadata, epoch persistence, registry, typed constructor, and legacy
   constructor.
2. Migrate the primary event families and add registry/fixture tests.
3. Move first-party clients to generated envelope types and explicit unknown
   fallback.
4. Migrate remaining legacy producers incrementally.
5. Remove legacy double-read only after persisted pre-v2 logs are no longer a
   supported input.

## Non-goals

- Migrating every audit event producer in this change.
- Defining UI labels, display priority, or timeline item schemas.
- Replacing the raw event stream with a server-side UI projection.
- Introducing a second JSON Schema generation framework beside Rust/OpenAPI.
