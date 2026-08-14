---
title: RFC: Event-Ledger Web Synchronization and Browser-Local Read State
date: 2026-08-14
status: proposed
handle: rfc-observer-sync-agent-summary-read-markers
---

# RFC: Event-Ledger Web Synchronization and Browser-Local Read State

## Summary

Holon should treat the Web GUI as an independent consumer of the existing
per-Agent runtime event ledger. The canonical replay identity remains:

```text
(event_log_epoch, agent_id, event_seq)
```

Phase 3 does not introduce a second delivery sequence, an observer change
ledger, or server-owned browser read state. Instead it adds two strict recovery
surfaces around the event ledger:

```http
GET /api/agents/snapshot
GET /api/agents/{agent_id}/projection-snapshot
```

The authoritative roster snapshot discovers all Agents visible to the current
authorization scope and reports each Agent's event recovery window. The
per-Agent projection snapshot supplies one consistency boundary from which a
client can resume raw events. Existing event pages and streams continue to
carry the incremental log.

The browser stores raw events, pending hydration, derived info/verbose/debug
projections, unread baselines, and read markers in IndexedDB. Unread is a local
product state for one browser profile. It is derived from hydrated
`brief_created` events and is not a server-owned fact.

This design reuses the event ordering that is already implemented, supports
both compact and verbose views, and makes roster discovery, retention gaps,
epoch resets, authorization changes, and hydration failure explicit.

## Status And Implementation Boundary

This RFC replaces the previous design in this file. The previous design
proposed an observer-scoped sync token, `observer_change_seq`, Agent
incarnations, a separate `delivery_seq`, a delivery metadata API, and
server-owned read markers. Those mechanisms are not adopted.

This RFC is still a design decision. It does not implement the new endpoints,
storage constraints, migrations, event metadata, or Web GUI cutover.
Implementation must proceed in separately reviewable server and client slices.

## Current Contract

Holon already has the following relevant structure:

- per-Agent audit events have durable, immutable identity
  `(event_log_epoch, agent_id, event_seq)`;
- `event_seq` is allocated independently for each Agent and persists across
  ordinary daemon restarts;
- `GET /api/agents/{agent_id}/events` supports bounded durable replay with
  `before_seq` and `after_seq`;
- `GET /api/agents/{agent_id}/events/stream` uses `event_seq` as its SSE id and
  rejects a missing retained cursor with `cursor_not_found`;
- `GET /api/events/stream` is a live-only watcher with no durable global
  cursor;
- `GET /api/agents/list` returns lightweight Agent entries, but is not a strict
  all-or-nothing recovery snapshot;
- canonical Brief, Message, Transcript, WorkItem, task, and Agent state records
  are available through dedicated APIs;
- events commonly act as invalidations or stable entity references rather than
  complete copies of those canonical records; and
- the Web GUI already recovers known Agents by `event_seq` and locally derives
  timeline and unread state.

The existing ordering is sufficient for info, verbose, and debug projections.
The remaining gaps are discovery of Agents changed while the browser was
offline, a consistent projection bootstrap, durable hydration bookkeeping,
retention reset semantics, and explicit local unread ownership.

## Problem

The global event stream deliberately does not replay history. A browser that is
closed cannot learn from that stream that an Agent was created, deleted, or
made inaccessible. The current bootstrap list also does not provide a strict
snapshot contract or per-Agent event-window anchors.

For an already known Agent, raw event replay is durable, but a client cannot
safely claim that several independently fetched canonical APIs form one
continuous projection at a specific `event_seq`. A retention gap therefore
cannot be repaired by simply joining `/agents/list`, state, briefs, transcript,
and WorkItem responses and assigning the result an event cursor.

Hydration is also distinct from ingestion. Persisting an event envelope is
enough to advance the network replay cursor, but not necessarily enough to
render it or classify it for unread. If pending hydration is only held in
memory, a browser crash after cursor advancement can permanently omit data from
its projection.

Finally, unread differs by browser profile. A marker associated with an account
or authorization principal would merge reading performed on different devices.
A marker associated with an SSE connection or tab would not survive reconnect.
The service cannot infer a stable physical device identity from a request.

## Goals

- preserve the existing event identity and ordering contract;
- make Agent discovery authoritative after startup, reconnect, and
  authorization change;
- provide a real per-Agent projection consistency boundary for bootstrap and
  retention recovery;
- let info, verbose, and debug views share one raw event cache and cursor;
- make event ingestion and canonical-record hydration independently durable;
- define exact browser-local unread behavior when event history is complete;
- represent retention truncation honestly when exact historical unread cannot
  be reconstructed;
- keep the global SSE stream as a low-latency hint rather than a correctness
  boundary;
- preserve authorization and cache partitioning explicitly; and
- define capabilities and migration rules before client cutover.

## Non-goals

- redesigning or globally ordering `event_seq`;
- making the event ledger a complete event-sourced database;
- introducing `delivery_seq`, `observer_change_seq`, or another replay ledger;
- storing per-browser read markers on the server;
- sharing unread state across devices or browser profiles;
- putting full conversation history in the roster response;
- making filtered event pages safe as raw ingestion cursors;
- hiding retention loss behind a fabricated exact unread count;
- replacing canonical Brief, Message, Transcript, WorkItem, task, or Agent
  state APIs; or
- implementing the server or Web GUI changes in this RFC revision.

## Core Decisions

### Reuse The Existing Event Sequence

The event ledger is the only incremental ordering domain used by Phase 3.
Info, verbose, and debug views differ in presentation policy, not replay
identity.

A client stores one contiguous raw cursor per Agent. Display filters such as
`max_level` may reduce a history response for a user-visible view, but the
highest sequence in a filtered response must never be treated as proof that all
raw events through that sequence were ingested.

### Use Snapshot Discovery Instead Of An Observer Ledger

Phase 3 does not add a durable global change cursor. On every initial
connection and every successful global SSE reconnect, the client obtains an
authoritative roster snapshot. The global stream only causes earlier refresh
or per-Agent catch-up.

This intentionally pays the cost of reading a complete roster. A future
observer change ledger is justified only after measured Agent counts make the
strict roster snapshot a bottleneck.

### Keep Read State In The Browser

The reader boundary is the browser profile. IndexedDB state is shared by tabs
of that profile and remains independent across devices, browsers, profiles,
and private sessions.

Clearing site data removes the reading history. A fresh profile establishes an
explicit unread baseline instead of pretending that the service can recover
what that browser previously read.

### Hydrate Canonical Records

Events establish change order and stable references. Canonical records remain
owned by their domain stores and APIs. The client persists hydration work so
advancing its raw replay cursor cannot lose a referenced record.

### Require A Real Projection Snapshot

Normal first bootstrap and retention-gap recovery use the same per-Agent
projection snapshot. Its records and `snapshot_through_seq` must come from one
provable consistency boundary. Multiple ordinary API calls cannot be relabeled
as a snapshot.

## Terminology

### Runtime connection

The browser configuration that identifies one Holon server endpoint and its
authorization material. It is part of the local cache key but is not sent as a
client-selected authority identifier.

### Event log epoch

The durable identity of one runtime event database generation. Reopening the
same database preserves `event_log_epoch`; replacing or rebuilding it creates a
new epoch.

### Visibility scope

The effective set of Agents visible under the server-resolved authorization
context. The server represents it with an opaque `visibility_scope_id` for
cache partitioning. It is not a credential and does not grant access.

### Ingested-through cursor

The greatest raw `event_seq` such that every event through that sequence has
been durably stored locally without an identity conflict.

### Projection-ready cursor

The greatest `event_seq` through which all display-affecting events have been
classified and their required canonical records have been hydrated or
terminated by an explicit tombstone.

### Unread baseline

A local product boundary before which Brief events do not participate in the
current unread generation. It does not claim that the user read older content.

### Read marker

A raw event boundary through which the current browser profile has marked every
qualifying `brief_created` event read, subject to projection readiness. The
boundary may name a non-qualifying event; unread calculation still counts only
qualifying Brief events after it.

## Ownership Boundary

The server owns these canonical facts:

- `event_log_epoch` and immutable event identities;
- Agent identity, lifecycle, and authorization visibility;
- each Agent's committed event head and retained event floor;
- canonical Agent, Brief, Message, Transcript, WorkItem, task, and related
  records;
- the stable relationship between a Brief and its unique `brief_created`
  event; and
- strict roster and projection snapshot consistency.

The browser owns these derived facts:

- roster sorting, selection, and presentation state;
- the local raw event cache and info/verbose/debug projections;
- pending hydration and projection readiness;
- cached latest preview data;
- unread baseline, read marker, and truncation acknowledgement; and
- reset and stale-state presentation.

The server does not own a delivery ledger, observer read state, or exact unread
count in Phase 3.

## Agent Identity

Within one `event_log_epoch`, an `agent_id` is a durable identity and must not be
reused for a different Agent. This follows the existing Agent deletion
contract: deletion is irreversible and deleted Agent ids remain reserved.

The service must enforce this with the existing persistent Agent identity
record or an equivalent tombstone that Agent deletion does not remove:

- normal creation rejects an `agent_id` that was previously retired in the
  same epoch;
- deletion removes current availability but not the identity reservation; and
- migration backfills the registry from current Agent records and historical
  Agent audit scopes.

If an existing database cannot establish this invariant, it must rotate
`event_log_epoch` or remain on a legacy capability path. Phase 3 does not add an
Agent incarnation field to compensate for ambiguous identity reuse, and it does
not add an Agent restore operation.

## Capability Advertisement

The handshake must advertise the new contracts independently:

```text
agents.roster-snapshot.v1
agents.projection-snapshot.v1
events.projection-effect.v1
briefs.atomic-created-event.v1
```

A capability is advertised only after its storage and consistency invariants
are active for the current database. Merely registering a route is not enough.

Complete Phase 3 exact mode requires all four capabilities. The Web GUI must
not enable authoritative discovery without `agents.roster-snapshot.v1`, must
not install a recovery watermark without `agents.projection-snapshot.v1`, and
must not advance projection readiness through unknown events without
`events.projection-effect.v1`. Exact local unread requires
`briefs.atomic-created-event.v1` in addition to the other three. If any
required capability is absent, the affected behavior remains on an explicit
legacy or uncertain path rather than partially claiming the new contract.

## Authoritative Roster Snapshot

### Endpoint

```http
GET /api/agents/snapshot
```

Suggested response model:

```rust
struct AgentRosterSnapshot {
    contract_version: u32,
    runtime_id: String,
    event_log_epoch: String,
    visibility_scope_id: String,
    agents: Vec<AgentRosterEntry>,
}

struct AgentRosterEntry {
    agent: AgentListEntry,
    event_window: AgentEventWindow,
    latest_brief: Option<AgentLatestBrief>,
}

struct AgentEventWindow {
    event_head_seq: u64,
    oldest_retained_seq: Option<u64>,
}

struct AgentLatestBrief {
    brief_id: String,
    created_event_seq: Option<u64>,
    created_at: DateTime<Utc>,
    preview: String,
}
```

The browser combines this response with its runtime connection identity when
forming local cache keys.

### Field Semantics

`runtime_id` is the stable public identity of the runtime installation. It
distinguishes a replaced server at the same configured URL from an ordinary
restart and is also returned by the projection snapshot. It is not a secret.

`visibility_scope_id` is generated by the server from stable runtime identity,
the server-resolved principal or authority, normalized visibility entitlement,
and a visibility policy generation. Credential rotation with unchanged
entitlement should keep it stable. A principal, entitlement, or policy change
must rotate it. Local unauthenticated mode uses a runtime-local public scope.

`event_head_seq` is the greatest committed `event_seq` visible in the response
read view. It must not come from an in-memory watcher or the next value in a
sequence allocator.

`oldest_retained_seq` is the first raw event still replayable in that same read
view. An Agent with no events has `event_head_seq = 0` and
`oldest_retained_seq = None`. Retention may advance after the response, so the
event page's `cursor_not_found` response remains authoritative.

`latest_brief` is derived from canonical Brief storage, not a second UI-summary
table. `preview` has a documented length limit. Full Brief content remains
available from the canonical Brief APIs.

Historical Briefs that cannot be linked to a unique retained
`brief_created.event_seq` use `created_event_seq = None`. Such records do not
participate in exact unread calculation.

### Consistency

The roster snapshot is authoritative for membership, authorization visibility,
and event-window anchors. In Phase 3 its visibility set is the same set of
active public Agents exposed by the existing remote-access Agent list and
global event stream. Private child Agents are not added to the Web roster.
Future expansion to a broader authorized visibility model requires an explicit
contract revision and visibility policy generation change. The roster is not a
conversation projection snapshot and does not allow the client to skip event
replay.

The following must be read under one consistent database view or an equivalent
pinned snapshot:

- the identity registry;
- authorization-filtered Agent membership;
- each Agent's event head and retained floor; and
- latest Brief identity and bounded preview.

The response is all or nothing. Failure to assemble one Agent must fail the
whole request. Returning a partial list would cause a client to interpret
omitted Agents as deleted or inaccessible.

The first implementation defines an explicit maximum Agent count, response
size, and request timeout. If pagination later becomes necessary, pages must be
bound to a server-pinned snapshot token. Independently read pages cannot form an
authoritative roster.

`GET /api/agents/list` remains available during migration but is not a recovery
contract.

## Per-Agent Projection Snapshot

### Endpoint

```http
GET /api/agents/{agent_id}/projection-snapshot
```

Suggested response model:

```rust
struct AgentProjectionSnapshot {
    contract_version: u32,
    runtime_id: String,
    event_log_epoch: String,
    visibility_scope_id: String,
    agent_id: String,
    snapshot_through_seq: u64,
    event_head_seq: u64,
    oldest_retained_seq: Option<u64>,
    projection: AgentCanonicalProjection,
}
```

`AgentCanonicalProjection` contains the current canonical facts and stable
revision anchors required to build the compact Agent card and current info
projection. It includes at least:

- Agent lifecycle and posture state;
- current WorkItem and conversation revision anchors;
- latest Brief identity and bounded preview;
- tombstones or absence markers needed to terminate hydration; and
- stable references for any full records that remain available through batch
  APIs.

It need not contain an unbounded verbose timeline or full Brief text. The
snapshot explicitly marks raw timeline history at or before
`snapshot_through_seq` as outside the incremental projection baseline. A
selected conversation may separately load bounded retained history for display,
but that history page neither changes the snapshot watermark nor proves raw
cursor continuity.

### Consistency Boundary

The projection and `snapshot_through_seq` must represent one consistency
boundary. Every event with `event_seq <= snapshot_through_seq` that affects
current canonical state must already be reflected by the projection or one of
its revision anchors. Historical-only timeline entries before the boundary are
not reconstructed by this invariant; their omission is represented by the
explicit history boundary above.

The service may implement this with:

- one SQLite read transaction over canonical records and audit events;
- a persisted projection generation committed atomically with the event
  watermark; or
- a durable outbox that provides an equivalent ordered visibility boundary.

If current canonical state spans memory and storage without such a boundary,
the endpoint capability must remain disabled until the boundary exists.

`event_head_seq` may be greater than `snapshot_through_seq`, but it must name a
committed event available through the event page. The client atomically installs
the projection at `snapshot_through_seq`, then replays
`(snapshot_through_seq, event_head_seq]` and all later events.

## Event Envelope Requirements

Each event family that can affect the Web projection must provide:

- a stable entity reference;
- a create, update, delete, or invalidation operation;
- a revision or version anchor when the canonical record supports one; and
- a projection effect classification.

Suggested projection effect values are:

```text
none
display_invalidation
```

`projection_effect` is an additive top-level field on `StreamEventEnvelope`,
published by OpenAPI and shared by event pages and SSE. The runtime event
registry descriptor is its source of truth. Enabling
`events.projection-effect.v1` means every served envelope has the field,
including historical records: typed events derive it from the registry, while
legacy or otherwise unclassified events default conservatively to
`display_invalidation`. Introducing the field increments the envelope contract
version; clients still accept older envelopes under the compatibility rules
below.

A known self-contained event may be applied directly. A reference event creates
or updates durable hydration work. A delete event must include enough tombstone
identity to complete projection without fetching a record that no longer
exists.

Unknown events remain stored as diagnostic evidence. An unknown event marked
`none` does not block projection readiness. An unknown event marked
`display_invalidation` blocks readiness until a supported snapshot or newer
client resolves it. If an older envelope omits `projection_effect`, a client
may use its static registry only for a known schema and version; an unknown
event without the field is treated as `display_invalidation`. An unknown
envelope contract version is not safely classifiable and therefore blocks the
Agent projection.

## Canonical Record And Event Commit Order

A canonical record referenced by an observable event must not become readable
after that event.

The preferred write boundary is one transaction that:

1. writes the canonical record;
2. allocates the Agent's `event_seq`;
3. writes the event or durable outbox item; and
4. commits both visibility changes together.

If the record and event use different stores, a durable outbox may publish the
event only after the record is readable.

### Brief Linkage

For each Brief, this relationship is unique and immutable:

```text
(agent_id, brief_id) -> created_event_seq
```

The Brief record, sequence allocation, and unique `brief_created` event must be
committed atomically or through an equivalent idempotent outbox. Retrying Brief
publication must not allocate a second sequence.

This field is event ordering metadata, not `delivery_seq`, and it does not
create a separate delivery ledger.

Existing Briefs may be backfilled from a unique historical `brief_created`
event. If no unique event can be proved, `created_event_seq` remains absent and
the Agent stays in legacy or uncertain unread mode for that history.

## Discovery State Machine

The browser uses the following startup and reconnect sequence:

1. open the global `GET /api/events/stream` connection and buffer hints;
2. after the stream reports open, request `GET /api/agents/snapshot`;
3. validate runtime identity, epoch, and visibility scope;
4. atomically apply the complete roster snapshot;
5. purge cached Agents omitted from the authoritative snapshot;
6. register or refresh per-Agent recovery work;
7. if any hint arrived while the snapshot was in flight, mark the roster dirty
   and perform one coalesced refresh; and
8. mark discovery fresh only after that refresh state is settled.

Every successful global SSE reconnect repeats the snapshot step. Agent-created,
Agent-deleted, visibility, and ordinary Agent event notifications are all hints;
none is individually required for correctness.

Heartbeat and timeout behavior must detect silent half-open streams. A low-rate
full reconciliation may be used as a safety net, but high-frequency polling is
not the primary protocol.

If the roster request fails, the browser keeps the previous roster but marks it
stale. It must not delete Agents based on an incomplete or failed response.

## Per-Agent Synchronization State Machine

The local raw cursor key is:

```text
(runtime connection, runtime_id, visibility_scope_id, event_log_epoch, agent_id)
    -> ingested_through_seq
```

For a newly visible Agent without cache:

1. fetch the per-Agent projection snapshot;
2. atomically install the projection and set
   `ingested_through_seq = snapshot_through_seq` and
   `projection_ready_through_seq = snapshot_through_seq`;
3. establish a fresh unread baseline at `snapshot_through_seq`;
4. replay raw events after the snapshot boundary; and
5. connect or continue the per-Agent live stream after contiguous catch-up.

For an Agent with an existing contiguous cache:

1. compare the roster event window with the local cursor;
2. replay raw events with `after_seq=ingested_through_seq`;
3. persist each immutable envelope before advancing the cursor;
4. continue until at least the roster's observed `event_head_seq` is reached;
   and
5. process later live events in the same order.

Info, verbose, and debug views use the same raw cache. Switching view level only
changes projection and hydration demand. Selected conversation history may load
additional bounded records, but those pages do not establish the raw cursor.

Duplicate immutable events are idempotent. Different content for the same
`(event_log_epoch, agent_id, event_seq)` is a protocol error that stops the
Agent projection and triggers reset.

## Durable Hydration

The browser stores these items in one IndexedDB transaction where applicable:

- the event envelope;
- its identity and classification;
- pending canonical-record references;
- the updated contiguous ingestion cursor; and
- any directly applicable projection changes.

Network ingestion may advance after the envelope and pending hydration are
durable. The independent `projection_ready_through_seq` advances only when all
display-affecting events through that point are resolved.

If a canonical API returns a newer revision than the event reference, the
client applies the newer record. Events are invalidations, not demands to
reconstruct an obsolete revision.

A referenced create or update record that remains missing after bounded retry
is projection divergence. The client refreshes the per-Agent projection
snapshot. If the mismatch remains, it displays a synchronization error rather
than silently dropping the event.

A browser restart scans durable pending hydration before declaring the
projection ready.

## Browser-Local Unread State

### Storage Model

The local reader key is:

```text
(runtime connection, runtime_id, visibility_scope_id, event_log_epoch, agent_id)
```

Suggested value:

```rust
struct LocalReadState {
    unread_baseline_seq: u64,
    read_through_event_seq: Option<u64>,
    certainty: ReadCertainty,
    history_truncated_before_seq: Option<u64>,
    acknowledged_truncation_before_seq: Option<u64>,
    updated_at: DateTime<Utc>,
}

enum ReadCertainty {
    Exact,
    Truncated,
}
```

Only successfully hydrated, user-facing `brief_created` events qualify as
unread. Operator messages, progress, tool calls, scheduler diagnostics, and
other internal events do not.

The unread set is:

```text
qualifying brief events where
  event_seq > max(unread_baseline_seq, read_through_event_seq or 0)
```

The result is a count of qualifying events, not the difference between two raw
sequences.

### Fresh Browser Policy

A fresh browser profile sets:

```text
unread_baseline_seq = projection_snapshot.snapshot_through_seq
read_through_event_seq = None
certainty = Exact
```

This means older history does not start as unread. It does not claim that the
user read that history.

### Advancing The Marker

The browser may advance `read_through_event_seq` only when:

- the Agent conversation is selected;
- the document is visible;
- the projection has no gap;
- catch-up has reached the current observed event head; and
- `projection_ready_through_seq` covers the candidate marker.

The update is a monotonic maximum. It may cross non-qualifying internal events,
but it may not cross an unresolved display invalidation.

Tabs in one browser profile merge marker updates through IndexedDB transactions
and BroadcastChannel or an equivalent local notification mechanism. Different
profiles do not merge.

No marker is sent to the server. A connection id, tab id, User-Agent, IP
address, or inferred physical device id is not a valid reader identity.

### Future Cross-Device State

If Holon later needs account-wide read state, it should be designed as a
separate product feature with the explicit meaning "read on any device." That
meaning differs from per-browser unread.

If server persistence for browser profiles is ever required, the browser must
generate an opaque durable `reader_id`, and the server key would be:

```text
(server-resolved authority, reader_id, event_log_epoch, agent_id)
```

That feature would also require reader registration, revocation, expiration,
and privacy-mode behavior. It is outside Phase 3.

## Retention And Reset

### Normal Catch-Up

If the epoch matches and the local cursor is at or after
`oldest_retained_seq - 1`, the client replays `after_seq` normally. Offline
Brief events remain available for exact unread calculation.

### Rich Cursor Error

`cursor_not_found` should include:

```text
event_log_epoch
oldest_retained_seq
event_head_seq
```

This lets the client distinguish a retained-prefix gap from an epoch change and
select the correct reset path.

### Per-Agent Retention Gap

An Agent-local reset begins when:

- the local cursor is less than `oldest_retained_seq - 1`;
- the event page or SSE endpoint returns `cursor_not_found`;
- one immutable event identity has conflicting content; or
- projection divergence cannot be repaired by bounded hydration retry.

Recovery is:

1. preserve the local read-state record temporarily;
2. discard the Agent's raw event and derived projection cache;
3. fetch and atomically install a per-Agent projection snapshot;
4. set `ingested_through_seq = snapshot_through_seq` and
   `projection_ready_through_seq = snapshot_through_seq`;
5. replay all events after that boundary;
6. record `history_truncated_before_seq`; and
7. resume live synchronization after contiguous catch-up.

If the effective local read boundary
`max(unread_baseline_seq, read_through_event_seq or 0)` is less than
`oldest_retained_seq - 1`, exact historical unread is unknowable. The client
retains any visible retained unread as a lower bound, changes `certainty` to
`Truncated`, and displays a truncation indicator instead of an exact badge.

The user may explicitly acknowledge the unknown history after opening and
catching up the conversation. That action records:

```text
acknowledged_truncation_before_seq = current_head
unread_baseline_seq = current_head
certainty = Exact
```

`Exact` then applies only to the new local generation after that boundary. The
client retains truncation metadata and does not claim to have reconstructed the
lost interval.

### Runtime Epoch Reset

When `event_log_epoch` changes, the browser:

- stops all per-Agent projections for the old epoch;
- clears the runtime scope's roster and session projection cache;
- does not migrate read markers to the new epoch;
- obtains a new authoritative roster;
- bootstraps each Agent from a projection snapshot;
- establishes fresh unread baselines; and
- displays one runtime-history-reset indication.

Old-epoch browser data may be garbage-collected asynchronously, but must never
be joined with the new epoch.

### Visibility Change

When `visibility_scope_id` changes, the browser clears the old scope's
accessible cache before exposing the new scope. An Agent omitted from an
authoritative roster is removed locally whether the cause is deletion or loss
of permission. The client must not retain inaccessible content while trying to
infer the reason.

If the Agent becomes visible later, it is bootstrapped as a fresh visible
Agent.

## Authorization And Privacy

Both snapshot endpoints use the same server-resolved remote-access
authorization as the canonical Agent APIs. Phase 3 lists only active public
Agents, matching `/api/agents/list`; the global `/api/events/stream` remains a
live hint channel rather than a roster authority. A requested per-Agent
snapshot must also pass the existing public-Agent authorization boundary.
Clients cannot request an arbitrary visibility scope.

Roster membership, latest Brief preview, event-window metadata, projection
records, errors, counts, and timing behavior must not reveal inaccessible
Agents.

An authorization failure is different from a transient snapshot failure. On
authentication or authorization failure, the Web GUI stops presenting old
cache as currently authorized data and requires reauthentication. On a
transient server error, it may present old data marked stale.

`visibility_scope_id` is opaque and non-secret. It is a cache partition, not a
bearer capability.

## Failure Behavior

### Roster Snapshot Failure

Keep the last complete roster, mark discovery stale, and retry with bounded
backoff. Never apply deletions from a partial response.

### Per-Agent Not Found

Refresh the authoritative roster. If the Agent is absent, purge its cache. If
it remains present, treat the error as a race or transient projection failure
and retry according to bounded policy.

### Unknown Event

Preserve the envelope. A known `projection_effect = none` allows readiness to
advance. An unresolved display invalidation blocks readiness and read-marker
advancement.

### Hydration Failure

Keep the envelope and pending hydration durable. Do not advance the relevant
projection-ready cursor or read marker. Retry, then use projection snapshot
repair, then expose a synchronization error if divergence remains.

### Local Transaction Failure

Do not advance the raw cursor unless envelope and pending hydration writes
commit. Replaying an already received event is safe because identity is
immutable and application is idempotent.

### Snapshot Assembly Failure

Fail the whole HTTP request. Neither endpoint may return a response that claims
a consistency boundary when one record or watermark failed to load.

## Migration

### Phase 0: Contract And Fixtures

- replace the old observer/delivery design with this RFC;
- add OpenAPI shapes and JSON fixtures for both snapshots;
- define projection-effect metadata and rich `cursor_not_found`; and
- add capability names to the handshake contract.

### Phase 1: Server Consistency Foundation

- add the durable Agent identity registry or tombstones;
- persist a stable public `runtime_id`;
- backfill identity history from Agent records and audit scopes;
- add immutable Brief-to-created-event linkage;
- make canonical record and event visibility atomic through a transaction or
  durable outbox; and
- keep capabilities disabled until migration verification succeeds.

### Phase 2: Recovery Endpoints

- implement the strict roster snapshot;
- implement the per-Agent projection snapshot;
- expose committed event head and retained floor metadata;
- enrich cursor errors; and
- enable capabilities only after consistency and authorization tests pass.

### Phase 3: Web Local State

- key IndexedDB by runtime connection, runtime identity, visibility scope,
  epoch, and Agent;
- persist raw envelopes, pending hydration, ingestion cursor, projection-ready
  cursor, and local read state;
- share marker updates across tabs; and
- expose legacy, exact, stale, and truncated UI states.

Existing `holon.webGui.rosterActivityByRemote.v1` localStorage values and legacy
IndexedDB fields such as `lastReadDeliverySeq` and `lastUnreadDeliverySeq` are
not imported as exact Phase 3 markers because they are not partitioned by
`runtime_id`, `visibility_scope_id`, and `event_log_epoch`, and their counters
cannot prove retained event completeness. On first successful Phase 3
bootstrap, the client may preserve timestamps as non-authoritative sorting
hints, establishes a fresh unread baseline from the projection snapshot, and
then deletes the legacy read fields for that remote. This one-time reset is
shown as a local unread-state migration, not as a runtime history reset.

### Phase 4: Discovery And Recovery Cutover

- adopt global SSE open/reconnect plus authoritative roster refresh;
- share one raw ledger across info, verbose, and debug views;
- use projection snapshots for new Agents and retention reset;
- handle epoch and visibility reset; and
- retain old `/agents/list` clients during the compatibility window.

### Phase 5: Acceptance And Cleanup

- pass offline, multi-tab, retention, epoch, authorization, and hydration fault
  tests;
- remove any unimplemented observer sync, delivery API, and server read-marker
  descriptions from generated client plans; and
- delete legacy Web paths only after supported server/client combinations are
  explicit.

## Compatibility

During migration:

- old servers continue to expose `/agents/list`, state, Brief, and event APIs;
- new fields on existing event and Brief records are additive;
- new Web clients inspect handshake capabilities before using exact recovery;
- a new Web client against an old server remains in legacy or uncertain mode;
- an old Web client against a new server continues using existing endpoints;
- epoch changes force bootstrap rather than field-by-field migration; and
- no client assumes that route existence proves snapshot consistency.

The unimplemented endpoints from the superseded design are not compatibility
obligations:

```text
POST /api/observer/sync
PUT  /api/observer/agents/{agent_id}/read-marker
GET  /api/agents/{agent_id}/deliveries
```

They should not be implemented as aliases for the new contract.

## Invariants

1. Event identity is `(event_log_epoch, agent_id, event_seq)`.
2. One event identity has immutable content.
3. `agent_id` is not reused for a different Agent within one epoch.
4. A roster snapshot is complete for the active-public visibility scope or
   fails entirely.
5. Roster event head and retained floor name committed events in one read view.
6. A projection snapshot and `snapshot_through_seq` share one consistency
   boundary.
7. A client applies only events greater than an installed snapshot boundary.
8. Raw ingestion cursor advances only after durable envelope persistence.
9. Projection readiness advances only after display invalidations are resolved.
10. One Brief links to at most one immutable `brief_created.event_seq`.
11. Unread counts hydrated qualifying Brief events, not raw sequence distance.
12. Browser read markers are monotonic within one local reader key.
13. Read markers never cross unresolved display invalidations.
14. Retention loss is represented as truncation, not fabricated exact history.
15. Epoch and visibility changes partition or clear local state before reuse.
16. Global SSE is a hint and never the sole correctness source.

## Acceptance Matrix

### Identity And Storage

- ordinary daemon restart preserves epoch and event identities;
- database replacement rotates the epoch;
- normal creation cannot reuse a retired Agent id in the same epoch;
- deletion remains irreversible and the deleted Agent id stays reserved;
- a duplicate Brief publication does not allocate another created-event
  sequence; and
- a record is readable no later than its referencing event becomes observable.

### Roster Discovery

- an Agent created while the browser is offline appears after reconnect;
- a deleted or newly inaccessible Agent is purged after a complete snapshot;
- a partial assembly failure does not remove any cached Agent;
- a hint received during snapshot assembly causes one coalesced refresh;
- global SSE reconnect always precedes a fresh discovery state; and
- inaccessible Agents do not leak through metadata or errors.

### Per-Agent Recovery

- a cached Agent catches up all raw events after its contiguous cursor;
- a new Agent installs a projection snapshot then replays only later events;
- info and verbose views share one cursor without losing verbose events;
- filtered history is never accepted as proof of raw continuity;
- duplicate immutable events are idempotent; and
- conflicting immutable content stops projection and resets.

### Hydration

- a browser crash after event persistence but before hydration resumes pending
  work on restart;
- a newer canonical revision satisfies an older invalidation;
- an unexplained missing record triggers retry and snapshot repair;
- a delete tombstone completes without fetching a deleted record;
- diagnostic unknown events do not block readiness; and
- unknown display invalidations do block readiness and read advancement.

### Local Read State

- a fresh profile starts with no historical unread before its baseline;
- two tabs in one profile converge by monotonic maximum;
- different profiles and devices retain independent unread state;
- only hydrated user-facing Brief events produce unread;
- internal events may be crossed but do not increment unread; and
- a hidden or stale conversation does not advance the marker.

### Retention And Reset

- a retained cursor catches up exactly;
- `cursor_not_found` supplies `event_log_epoch`, `oldest_retained_seq`, and
  `event_head_seq`;
- a retention gap installs a canonical projection snapshot;
- lost history produces truncated certainty rather than an exact count;
- explicit acknowledgement starts a new exact generation without rewriting the
  historical claim;
- epoch reset discards old projection and read state; and
- visibility-scope rotation clears inaccessible cache before display.

### Compatibility

- new Web plus old server selects legacy or uncertain mode;
- old Web plus new server continues through old endpoints;
- capabilities remain disabled until migrations and consistency checks pass;
- route presence without capability does not enable the new state machine; and
- no delivery or server read-marker endpoint is required for cutover.

## Observability

The server should report:

- roster snapshot duration, Agent count, response bytes, and failure reason;
- projection snapshot duration, boundary, head distance, and failure reason;
- capability enablement and migration verification status;
- cursor-not-found counts by Agent scope without exposing private identifiers;
- record/event outbox lag or transaction failure; and
- identity-registry or Brief-linkage invariant violations.

The Web GUI should report locally diagnosable state for:

- discovery fresh or stale;
- ingested-through and projection-ready cursors;
- pending hydration count;
- exact, truncated, or legacy unread certainty;
- last reset reason; and
- current epoch and visibility cache partition.

Logs and diagnostics must not include credentials, capability secrets, full
private Brief text, or inaccessible Agent metadata.

## Rejected Alternatives

### Add `delivery_seq`

A separate delivery sequence would order only user-facing Briefs. The verbose
view would still require `event_seq`, creating two recovery domains and an
additional linkage and migration problem. Exact unread only needs stable Brief
to event linkage, not a new sequence allocator or delivery ledger.

### Use `delivery_seq` As The Only Cursor

This loses progress, tool, lifecycle, diagnostic, and other verbose events.
Info and verbose would no longer be projections over the same recovered input.

### Add An Observer Change Ledger Now

A durable global delta cursor would reduce roster snapshot cost, but adds
projection generation, compaction, authorization tombstones, token security,
and another recovery protocol. Phase 3 should first use complete snapshots and
add deltas only after measured scale requires them.

### Store Read Markers By Observer Principal

This merges all devices for one principal and contradicts the desired
per-browser unread behavior. It also cannot distinguish browser profiles.

### Store Read Markers By Connection Or User-Agent

Connections are ephemeral and User-Agent values are neither unique nor stable.
Neither is a durable reader identity.

### Make The Event Ledger Fully Event-Sourced

Re-encoding every canonical record into replay-complete events would greatly
expand the migration and retention contract. Stable references plus durable
hydration preserve domain ownership while keeping event ordering explicit.

### Reuse `/agents/list` As The Snapshot

The current list path permits placeholder behavior and does not promise one
all-or-nothing visibility and event-window read boundary. Changing it in place
would silently strengthen a compatibility endpoint. A strict endpoint makes
the recovery contract explicit.

### Compose Ordinary APIs Into A Projection Snapshot

Independent calls may observe different commits. Assigning their joined result
a cursor can skip events whose effects are absent from the composition. A
snapshot watermark is valid only with a proven consistency boundary.

### Treat Retained History As Exact

After a gap, the client cannot know which qualifying Brief events were deleted.
Showing an exact count would be a false statement. Truncation must remain
visible until the user starts a new acknowledged generation.

## Proposed Decision

Adopt the existing per-Agent event ledger as the Phase 3 synchronization
backbone.

1. Keep `(event_log_epoch, agent_id, event_seq)` as the only incremental replay
   identity.
2. Add a strict authoritative roster snapshot for discovery.
3. Add a consistent per-Agent projection snapshot for bootstrap and reset.
4. Make canonical-record hydration durable and ordered relative to observable
   events.
5. Link Briefs immutably to their unique `brief_created.event_seq`.
6. Store unread baselines and read markers in the browser profile.
7. Represent retention, epoch, and authorization reset explicitly.
8. Do not implement `delivery_seq`, observer deltas, Agent incarnations, or
   server-owned browser read markers in Phase 3.
