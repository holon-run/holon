---
title: RFC: Observer Sync, Canonical Agent Summary, and Monotonic Read Markers
date: 2026-08-12
status: proposed
handle: rfc-observer-sync-agent-summary-read-markers
---

# RFC: Observer Sync, Canonical Agent Summary, and Monotonic Read Markers

## Summary

Holon should add a durable observer synchronization surface for first-party
clients:

```text
POST /api/observer/sync
PUT  /api/observer/agents/{agent_id}/read-marker
GET  /api/agents/{agent_id}/deliveries
```

The sync response returns a coherent roster of canonical agent summaries
composed with observer-scoped read state. It uses an opaque, runtime-scoped sync
token and returns either a full snapshot or all agent summaries changed since
the previous token.

User-facing conversation delivery is a separate append-only domain from raw
runtime events. Each delivery receives a per-agent `delivery_seq`. The latest
delivery, its bounded preview, the observer's monotonic read marker, and the
exact unread count are server-owned facts.

This contract fixes the first-party Web GUI case where the browser is offline
while briefs are delivered. A reconnecting client no longer has to infer roster
previews or unread counts from a live-only global SSE stream, browser-local
counters, or partial raw event replay.

## Status And Implementation Boundary

This RFC is a design decision only. It does not implement the endpoints,
storage tables, migrations, or Web GUI cutover.

The first implementation should be reviewed as separate server, client, and
frontend slices after this RFC is accepted.

## Problem

Holon currently exposes three useful but different surfaces:

- `GET /api/agents/list` returns lightweight public agent entries;
- `GET /api/agents/{agent_id}/state` returns an agent bootstrap snapshot; and
- raw per-agent event pages and streams use
  `(event_log_epoch, agent_id, event_seq)` for replay.

The global `GET /api/events/stream` endpoint is deliberately live-only. It has
no global replay cursor. If it disconnects or the client is offline, the client
must recover each agent separately from its last contiguous `event_seq`.

The Web GUI currently derives roster activity and unread state locally:

- `brief_created` events increment a browser-local unread counter;
- a local read marker stores the last observed brief event sequence;
- the counter and marker are persisted in browser storage;
- the latest preview is hydrated by joining event payloads to brief records;
- agent list presentation is assembled from list, state, work-item, and brief
  requests.

This works as an online optimization but is not a complete synchronization
contract.

When the browser is closed or offline:

1. it does not receive global SSE events;
2. `/agents/list` does not expose a latest delivery cursor or preview;
3. there is no server-owned observer read marker;
4. an empty or retained raw event window cannot prove how many user-facing
   deliveries occurred; and
5. a second browser cannot share the first browser's local read state.

The client can backfill raw events, but raw event completeness is not the same
as a canonical user-facing delivery summary. Raw events include debug and
lifecycle records, are subject to event retention, and require payload
projection plus brief hydration before they become a conversation delivery.

## Goals

- restore exact unread state and latest preview after arbitrary client offline
  periods, subject only to explicit delivery retention policy;
- define one canonical, lightweight agent summary for roster clients;
- define observer identity and read-state ownership without trusting a
  client-supplied observer id;
- make read-marker updates atomic, idempotent, and monotonic;
- use an opaque sync token that can evolve independently of event cursors;
- preserve raw per-agent events as the diagnostic and runtime replay surface;
- let global SSE remain a low-latency wake hint rather than the source of
  synchronization truth;
- keep authorization, trust, and public-agent visibility explicit;
- specify reset, retention, migration, and acceptance behavior before server
  implementation.

## Non-goals

- do not replace the raw runtime event stream;
- do not expose the runtime-wide raw audit sequence as an observer sync cursor
  or treat an agent-filtered `event_seq` view as a conversation delivery
  sequence;
- do not define the full semantic conversation item API or anchor pagination;
- do not include full transcript, message, brief, task, or WorkItem payloads in
  the roster sync response;
- do not make browser local storage authoritative;
- do not permit read markers to create ingress, wake an agent, or change agent
  lifecycle;
- do not infer observer identity from an arbitrary request body field;
- do not expose private or inaccessible agents through sync tombstones,
  counts, or timing side channels;
- do not require AG-UI or another third-party protocol for the first-party
  contract.

## Terminology

### Observer principal

The authenticated principal whose view and read state are being synchronized.
It is derived by the server from the authorization context.

Examples:

- the configured control credential in the current single-operator model;
- a future authenticated operator account;
- the deployment-local operator principal when control authentication is
  explicitly disabled.

An observer principal is not supplied as `observer_id` by the client.

### Observer scope

The stable authorization identity whose view and read state are synchronized:

```text
(observer_principal, tenant_or_workspace_authority)
```

The current single-operator runtime has no additional tenant or workspace
component. Runtime identity and visibility-policy generation are token
validation fields, not parts of observer scope: changing either may require a
reset snapshot for the same authenticated scope.

### Agent incarnation

A durable opaque generation for one logical creation of an `agent_id`.

Deleting and recreating the same `agent_id` creates a new
`agent_incarnation`. Delivery sequences and read markers do not cross
incarnations.

### Conversation delivery

A compact, durable record saying that Holon produced one user-facing
conversation delivery for an agent.

The initial source is a persisted user-facing `BriefRecord`. A raw
`brief_created` audit event is evidence about that delivery, but its
`event_seq` is not the delivery cursor.

### Delivery sequence

`delivery_seq` is a per-agent-incarnation append sequence assigned by the
storage transaction that commits the delivery.

It is independent from:

- `event_seq`, which orders raw audit events;
- `brief_id`, which identifies brief content;
- `turn_index`, which orders agent turns; and
- timestamps, which are for display rather than cursor identity.

### Read marker

The highest conversation delivery the observer has acknowledged as read for
one agent incarnation.

The marker advances monotonically and is represented as:

```text
read_through_delivery_seq
```

### Observer change sequence

`observer_change_seq` is a runtime-wide append sequence used only to invalidate
and synchronize compact observer-visible state.

It is not a raw event cursor and is never presented as a domain object id.

### Sync token

An opaque server-issued token binding an observer scope to a completed sync
watermark and token format version.

Clients persist and replay the token verbatim. They do not parse or synthesize
it.

## Existing Contract Boundaries

The new API complements rather than changes these accepted contracts:

- raw event identity remains
  `(event_log_epoch, agent_id, event_seq)`;
- an `event_log_epoch` change invalidates cached raw-event projections;
- `/api/events/stream` remains live-only and has no global cursor;
- `/api/agents/{agent_id}/events` remains the bounded raw replay surface;
- `/api/agents/{agent_id}/state` remains an agent-detail bootstrap surface;
- brief, message, and transcript batch-get endpoints remain content hydration
  surfaces; and
- event retention may make an old raw cursor return `cursor_not_found`.

Observer sync does not claim that raw history is complete. It claims that its
returned canonical summaries and observer read states are complete at the
returned sync watermark.

## Canonical Data Model

### Agent Incarnation

The runtime stores:

```rust
struct AgentIncarnation {
    agent_id: String,
    incarnation: String,
    created_at: DateTime<Utc>,
}
```

The exact table layout is an implementation choice. Required behavior:

- an existing agent keeps the same incarnation across daemon restarts;
- importing or reopening the same runtime database preserves it;
- deleting and recreating an agent id creates a different incarnation;
- migration assigns one incarnation to every existing agent; and
- clients treat an incarnation change as replacement, not an in-place update.

### Conversation Delivery

The canonical delivery row is observer-independent:

```rust
struct CanonicalConversationDelivery {
    agent_id: String,
    agent_incarnation: String,
    delivery_seq: u64,
    delivery_id: String,
    brief_id: String,
    kind: BriefKind,
    created_at: DateTime<Utc>,
    work_item_id: Option<String>,
    turn_id: Option<String>,
    related_message_id: Option<String>,
    related_task_id: Option<String>,
}
```

Required policy:

- `delivery_seq` starts at `1` within an agent incarnation;
- it increases for each committed user-facing delivery;
- the append owner assigns it; callers do not;
- `(agent_incarnation, delivery_seq)` is unique and immutable;
- `delivery_id` remains an opaque reference and does not encode order;
- `brief_id` resolves full delivery content through the existing brief API;
- a delivery and its sequence are committed atomically with the corresponding
  durable brief evidence and observer invalidation; and
- retries with the same canonical brief identity are idempotent and must not
  allocate another delivery sequence.

The observer-visible projection is:

```rust
struct ObserverConversationDelivery {
    delivery_seq: u64,
    delivery_id: String,
    brief_id: String,
    kind: BriefKind,
    created_at: DateTime<Utc>,
    preview: String,
    preview_truncated: bool,
    work_item_id: Option<String>,
}
```

It is returned only when the observer may fetch the referenced brief.
`preview` is derived at read time or from an equivalently scoped cache using
the same content authorization and redaction policy as brief fetch. It is not
stored in or shared through the canonical agent summary.

The initial implementation creates deliveries only for briefs that are valid
user-facing deliveries. Internal progress, debug traces, tool results, and
assistant execution narration do not become deliveries merely because they
exist in an event payload.

If a later product surface introduces another user-facing delivery type, it
must define its relationship to `BriefRecord` and preserve the one-sequence-per-
visible-delivery invariant.

### Canonical Agent Summary

`CanonicalAgentSummary` is a compact server projection from canonical runtime
facts:

```rust
struct CanonicalAgentSummary {
    agent_id: String,
    agent_incarnation: String,
    summary_revision: u64,
    changed_at: DateTime<Utc>,

    identity: AgentIdentityView,
    status: AgentStatus,
    lifecycle: AgentLifecycleHint,
    scheduling_posture: AgentPostureProjection,
    waiting_reason: Option<WaitingReason>,
    pending_count: u64,
    active_task_count: u64,

    current_work: Option<AgentCurrentWorkSummary>,
    active_workspace: Option<AgentWorkspaceSummary>,
    model: AgentListModelSummary,

    latest_operator_activity_at: Option<DateTime<Utc>>,

    event_log_epoch: String,
    latest_event_seq: Option<u64>,
}
```

`AgentCurrentWorkSummary` contains only the canonical current WorkItem id,
objective, scheduling state, readiness, and reason code. It does not copy plan
text, todo lists, or full WorkItem records.

The summary projection follows these rules:

- current work comes from canonical WorkItem focus and the shared scheduling
  read model;
- status, lifecycle, posture, pending count, task count, workspace, and model
  use the same canonical projections already consumed by HTTP/TUI;
- latest operator activity is a bounded display fact and is not a read marker;
- `event_log_epoch` and `latest_event_seq` allow existing clients to decide
  whether their per-agent raw-event cache needs catch-up or reset;
- `summary_revision` advances for semantic summary changes, not for reads; and
- the summary must not contain full brief text, transcript, messages, task
  output, WorkItem plan bodies, secrets, private agent state, or runtime-global
  model availability.

The server should expose one pure lifecycle/status summary projection used by
observer sync and any future replacement for `/agents/list`. Clients must not
recreate its precedence rules by joining multiple endpoints. Observer-visible
delivery data is composed around that core rather than embedded in it.

### Observer Read State

Read state is keyed by:

```text
(observer_scope_id, agent_id, agent_incarnation)
```

`observer_scope_id` is the durable identifier for the complete observer scope
`(observer_principal, tenant_or_workspace_authority)`. It must not be reduced
to the principal alone, even when the current deployment has only one
authority.

The durable record is:

```rust
struct ObserverReadState {
    observer_scope_id: String,
    agent_id: String,
    agent_incarnation: String,
    read_through_delivery_seq: u64,
    revision: u64,
    updated_at: DateTime<Utc>,
}
```

Each incarnation has a durable read-state lower bound. It is zero for an
ordinary newly created incarnation and `cutover_baseline_seq` for a migrated
incarnation. An absent row is equivalent to that lower bound, never
unconditionally to zero. Materializing observer membership and initializing
its marker to the incarnation lower bound must be atomic; projections must not
observe visible membership with an uninitialized marker.

The public projection is:

```rust
struct ObserverAgentReadProjection {
    read_through_delivery_seq: u64,
    latest_delivery_seq: Option<u64>,
    unread_count: u64,
    updated_at: Option<DateTime<Utc>>,
}
```

`unread_count` is the exact number of authorized delivery rows in the current
agent incarnation whose `delivery_seq` is greater than the read marker. It is
not computed as `latest_delivery_seq - read_through_delivery_seq`, because a
policy may exclude, redact, or retain delivery classes differently.
`latest_delivery_seq` is the greatest sequence currently visible to this
observer, not the canonical latest sequence.

Read state is observer-specific. It is never stored inside
`CanonicalAgentSummary`.

### Observer Agent Summary

The sync wire object composes the two projections:

```rust
struct ObserverAgentSummary {
    agent: CanonicalAgentSummary,
    latest_delivery: Option<ObserverConversationDelivery>,
    read: ObserverAgentReadProjection,
}
```

This distinction keeps agent lifecycle facts canonical while making unread
state explicitly relative to the authenticated observer.

### Observer Change Ledger

Every transaction that may change an observer-visible summary appends an
invalidation under a runtime-wide `observer_change_seq`. Each row records:

```rust
struct ObserverChange {
    observer_change_seq: u64,
    audience: ChangeAudience,
    agent_id: String,
    agent_incarnation: String,
    reason: ObserverChangeReason,
}

enum ChangeAudience {
    ObserverScope(ObserverScopeId),
}
```

`ObserverScopeId` is an internal non-secret identifier for the stable scope
defined above. A read-marker change is always targeted to exactly one scope.
Canonical changes produce one scoped row per materialized membership that can
see the incarnation. Delta selection filters rows by audience before
coalescing. The coalescing key is
`(observer_scope_id, agent_id, agent_incarnation)`; one observer's private
read-state update must neither wake nor appear in another observer's delta.

Change reasons include:

- agent creation, visibility change, or deletion;
- canonical agent summary change;
- conversation delivery append;
- observer read-marker advancement; and
- materialized visibility transition.

The ledger is an invalidation and synchronization index, not a second raw audit
feed. One sync delta returns the latest complete summary for every affected
agent rather than every intermediate mutation.

The append must occur in the same database transaction as the canonical
mutation or through the existing transactional index-outbox pattern with a
durable divergence repair path. A committed canonical change without a durable
observer invalidation is a contract failure.

## Observer Identity And Authorization

### Principal Derivation

The HTTP authentication layer derives a stable internal
`ObserverPrincipalId`. The derivation must:

- be stable for the lifetime of the credential or operator identity;
- never expose the raw bearer token;
- use a one-way keyed or cryptographic fingerprint when the current control
  token is the only identity source;
- use a fixed deployment-local operator principal only when authentication is
  explicitly disabled; and
- rotate scope when authorization is revoked or materially changed.

The client cannot select another observer by sending a header, query parameter,
or JSON field.

### Visibility

Sync includes only agents authorized by the current observer scope.

For the current runtime this means public agents accepted by
`public_agent_identity`. Future private or multi-user agents must use the same
authorization policy as their direct read endpoints.

When an agent becomes inaccessible:

- the authorization mutation transaction consults a materialized visibility
  membership keyed by `(observer_scope_id, agent_id, agent_incarnation)`;
- it appends a scoped `visibility_removed` change only for memberships that
  existed immediately before the mutation, then removes those memberships;
- the client receives a tombstone only from that scoped transition record;
- a newly authenticated principal must not learn that the agent exists;
- read-marker endpoints return the same not-found boundary as direct agent
  reads; and
- sync tokens from a different stable scope are rejected rather than
  silently narrowed.

Granting visibility records membership and appends a scoped
`visibility_added` change. The membership table is canonical synchronization
state, not a reconstruction from the current policy. This makes removals
precise without embedding a full historical visible-agent set in every token.
Policy migration must materialize old and new membership in one transaction or
use a resumable generation switch that exposes neither mixed membership nor
unscoped tombstones.

### Sync Token Security

A sync token:

- is opaque;
- has a small version-neutral envelope whose stable observer scope, runtime
  instance id, token format discriminator, and integrity key id can be
  authenticated before format-specific payload decoding;
- is integrity protected; format-specific payload may additionally reference
  server-side token state;
- contains or references token format version, stable observer scope, runtime
  instance id, visibility-policy generation, projection generation, and
  `observer_change_seq` watermark;
- is not an authentication credential;
- is useless without matching request authorization;
- must not be accepted across observer scopes; and
- may be rotated without changing the endpoint shape.

Clients may log only a bounded token fingerprint, never the full token.

The deployment must retain a reset-verification keyring and the version-neutral
envelope decoder independently of the replaceable runtime database. Every key
that signed a token still within the documented reset compatibility horizon
must remain available after database replacement. Retiring a payload format
must retain enough envelope parsing and verification support to authenticate
its stable scope and return `token_version_retired`. Operators must not
advertise `observer.sync.v1` across a database replacement unless this reset
verifier survives the replacement. A token outside that explicit compatibility
horizon may fail as a generic invalid token, but the server must not guess its
scope or return roster data.

Validation order is normative:

1. authenticate and derive the current stable observer scope;
2. parse the version-neutral envelope and validate its integrity with the
   reset-verification keyring;
3. if the token's stable observer scope differs, return an authorization error
   with no roster data;
4. if the stable scope matches but the runtime instance differs, return a
   successful `runtime_replaced` reset snapshot without resolving old
   database-local token state;
5. if the token payload format is retired, return a successful
   `token_version_retired` reset snapshot; and
6. otherwise decode or resolve the format-specific payload and, when
   visibility-policy generation, projection generation, or retained watermark
   differs, return the corresponding successful reset snapshot.

This ordering makes `runtime_replaced` and `policy_changed` reachable without
allowing a credential or tenant boundary to reset into another scope.

## Sync API

### Request

```http
POST /api/observer/sync
Authorization: Bearer ...
Content-Type: application/json
```

```json
{
  "since": "opaque-token-or-null"
}
```

`since` is optional. Omitting it requests a snapshot.

The first version intentionally has no client-supplied agent list, observer id,
raw event cursor, or pagination limit. The response is compact and coalesced by
agent. If deployments later need roster pagination, it must preserve one
snapshot watermark across pages rather than returning mixed-time slices.

### Snapshot Response

```json
{
  "mode": "snapshot",
  "sync_token": "opaque-token",
  "reset_reason": null,
  "agents": [
    {
      "agent": {
        "agent_id": "holon-web",
        "agent_incarnation": "agent_gen_...",
        "summary_revision": 42,
        "changed_at": "2026-08-12T03:20:00Z",
        "identity": {},
        "status": "idle",
        "lifecycle": {},
        "scheduling_posture": {},
        "waiting_reason": null,
        "pending_count": 0,
        "active_task_count": 0,
        "current_work": null,
        "active_workspace": {},
        "model": {},
        "latest_operator_activity_at": "2026-08-12T03:10:00Z",
        "event_log_epoch": "epoch_...",
        "latest_event_seq": 847
      },
      "latest_delivery": {
          "delivery_seq": 19,
          "delivery_id": "delivery_...",
          "brief_id": "brief_...",
          "kind": "success",
          "created_at": "2026-08-12T03:19:59Z",
          "preview": "PR merged and verification passed.",
          "preview_truncated": false
      },
      "read": {
        "read_through_delivery_seq": 17,
        "latest_delivery_seq": 19,
        "unread_count": 2,
        "updated_at": "2026-08-12T02:00:00Z"
      }
    }
  ],
  "removed_agents": []
}
```

The exact nested DTOs should be generated from Rust/OpenAPI. The example is
illustrative, not a second schema source.

### Delta Response

For a valid `since` token:

```json
{
  "mode": "delta",
  "sync_token": "new-opaque-token",
  "reset_reason": null,
  "agents": [
    {
      "agent": {},
      "read": {}
    }
  ],
  "removed_agents": [
    {
      "agent_id": "retired-agent",
      "agent_incarnation": "agent_gen_old"
    }
  ]
}
```

The response includes:

- the latest complete `ObserverAgentSummary` for every authorized agent changed
  after the old token and at or before the new token watermark;
- scoped tombstones carrying the removed incarnation for agents deleted or
  made inaccessible after previously being visible in this scope; and
- a new token even when no summaries changed.

Intermediate revisions may be coalesced. A client that needs raw transition
history uses the per-agent event API.

The client keys cached summaries by `(agent_id, agent_incarnation)`. It applies
all tombstones to that exact key, then upserts returned summaries. A tombstone
for an old incarnation cannot remove a simultaneously returned replacement
incarnation with the same `agent_id`.

### Snapshot Consistency

The server builds a response from one database read snapshot:

1. authorize and derive observer scope;
2. begin a consistent read transaction;
3. validate or resolve `since`;
4. capture the observer change high watermark visible to that transaction;
5. select visible agents or affected agent ids;
6. project canonical summary and observer read state from the same snapshot;
7. include same-scope tombstones;
8. issue a token for the captured high watermark; and
9. commit/close the read transaction before writing the response.

A summary must not describe state newer than the returned token watermark.

If projection spans asynchronous runtime-only state, that state must first have
a canonical durable read projection. Sync must not mix a durable token with
unversioned in-memory facts.

### Empty Delta And Wake Hints

An empty delta is valid:

```json
{
  "mode": "delta",
  "sync_token": "new-or-equivalent-opaque-token",
  "reset_reason": null,
  "agents": [],
  "removed_agents": []
}
```

The global SSE stream may continue to wake the Web GUI early. On any relevant
global event, reconnect, lag, visibility resume, network resume, or periodic
staleness check, the client calls `/observer/sync` with its last token.

The client never increments unread directly from global SSE. SSE is a hint;
sync is authority.

Long polling may be added later as an optional `timeout_ms` request field. It
is not required for the first implementation.

### Reset Behavior

A token may require a full snapshot when:

- the token format is no longer supported;
- the token watermark is older than the retained observer change prefix;
- the runtime database was replaced or rebuilt;
- visibility-policy version changed in a way that cannot be represented as a
  safe delta; or
- durable divergence repair invalidated the prior watermark.

For a token from the same stable authenticated scope, the endpoint returns a
successful snapshot with:

```json
{
  "mode": "snapshot",
  "reset_reason": "token_expired"
}
```

Allowed reset reasons are stable machine-readable values:

- `token_expired`
- `runtime_replaced`
- `policy_changed`
- `projection_rebuilt`
- `token_version_retired`

Scope mismatch is not a reset. It is an authorization error because returning a
snapshot could leak another scope's existence or permit accidental credential
mixing.

## Delivery Metadata API

```http
GET /api/agents/{agent_id}/deliveries
    ?before_delivery_seq=<seq>
    &after_delivery_seq=<seq>
    &limit=<n>
    &order=asc|desc
```

This endpoint returns compact observer-visible delivery records only. It
supports:

- inspecting the exact delivery cursor named by a read marker;
- hydrating roster or notification history without scanning raw events;
- verifying unread-count behavior in tests; and
- future notification-center UI.

It does not replace the semantic conversation API planned separately. Full
content remains available from the referenced brief endpoint.

The endpoint applies the current observer's delivery authorization and returns
`ObserverConversationDelivery`, including an observer-authorized preview. It
does not expose canonical rows that the observer cannot fetch.

The response includes:

```rust
struct DeliveryPageResponse {
    agent_id: String,
    agent_incarnation: String,
    deliveries: Vec<ObserverConversationDelivery>,
    oldest_delivery_seq: Option<u64>,
    newest_delivery_seq: Option<u64>,
    latest_delivery_seq: Option<u64>,
    has_older: bool,
    has_newer: bool,
    order: DeliveryPageOrder,
    limit: usize,
}
```

Delivery paging cursors are not sync tokens.

## Read-Marker API

### Request

```http
PUT /api/observer/agents/{agent_id}/read-marker
Authorization: Bearer ...
Content-Type: application/json
```

```json
{
  "agent_incarnation": "agent_gen_...",
  "read_through_delivery_seq": 19
}
```

The incarnation is required to prevent a delayed request from marking a newly
recreated agent as read.

### Mutation Semantics

The read marker is a monotonic position in the canonical per-incarnation
delivery sequence. A requested non-zero marker must name a delivery currently
visible to the observer, but it acknowledges every sequence position `<= N`,
including invisible rows between previously visible deliveries. Thus an
observer that can see delivery `19` may mark `19` even if delivery `18` is
invisible. A later authorization grant for `18` does not turn it into unread
content. Authorization changes may reveal historical content, but they do not
move the marker backward.

In one transaction, the server:

1. authorizes the agent and derives observer principal;
2. verifies that `agent_incarnation` is current;
3. verifies that the requested sequence is `0` or identifies a currently
   authorized delivery in that incarnation, without exposing whether a failed
   lookup was absent, unauthorized, beyond the canonical head, or pruned;
4. reads the current marker;
5. stores `max(current, requested)`;
6. increments read-state revision only if the marker advances;
7. appends observer invalidation only if visible read state changes; and
8. returns the updated observer summary or read projection.

Concurrent writes therefore satisfy:

```text
stored_marker_after >= stored_marker_before
stored_marker_after >= every successfully acknowledged requested marker
```

Sending the same marker twice is idempotent. Sending an older marker succeeds
as a no-op and returns the current higher marker. It does not produce a
conflict, decrement unread, or append redundant invalidations.

### Validation

The server rejects:

- an unknown or inaccessible agent;
- a stale or foreign agent incarnation;
- a negative or malformed sequence;
- a non-zero sequence that is not available as a currently authorized
  delivery, including an absent, unauthorized, beyond-head, or pruned
  canonical sequence;
- credentials that cannot own observer read state.

Stable machine-readable errors:

| HTTP | Code | Meaning |
| --- | --- | --- |
| 400 | `invalid_read_marker` | Malformed marker request. |
| 403 | `auth_required` | No valid observer authentication. |
| 404 | `agent_not_found` | Unknown or inaccessible agent. |
| 409 | `agent_incarnation_changed` | Agent id names another incarnation. |
| 409 | `delivery_marker_unavailable` | Sequence cannot be accepted for this observer. |

`delivery_marker_unavailable` intentionally combines absent, beyond-head,
unauthorized, and non-retained cases. The response body, headers, and
documented behavior must not reveal which case occurred or expose the
canonical latest sequence. Implementations should avoid materially distinct
lookup paths that create a practical sequence-boundary timing oracle.

An `event_log_epoch` change does not by itself reset a delivery read marker.
Raw events and conversation deliveries have independent identities.

## Client State Machine

The first-party Web GUI uses:

```text
bootstrap:
    restore cached sync token and summaries
    POST /observer/sync { since: cached_token }
    replace or merge roster from response
    persist response atomically
    open global SSE as wake hint

global SSE event:
    debounce POST /observer/sync { since: token }
    apply returned complete summaries

SSE reconnect / browser online / visibility resume:
    sync first
    then treat live stream as current

open conversation:
    continue per-agent raw event/session recovery for conversation content
    do not mark read until current delivery is rendered and acknowledged

mark read:
    PUT read-marker with current agent incarnation and visible delivery_seq
    apply server response
    never decrement or locally overwrite a higher marker
```

The local cache is stale-while-revalidate display state. It may render cached
summaries immediately, but it must expose a syncing/stale state until a server
sync succeeds.

The client must stop:

- incrementing unread solely from `brief_created` SSE events;
- treating global SSE continuity as proof of offline completeness;
- storing the only authoritative read marker in localStorage or IndexedDB;
- using raw `newest_event_seq` as the delivery read marker; and
- constructing roster preview by racing multiple detail requests.

IndexedDB may continue to cache sync token, canonical summaries, read
projections, and per-agent conversation state for fast startup.

## Relationship To Raw Event Recovery

Observer sync and raw event recovery have separate jobs:

| Surface | Cursor | Purpose |
| --- | --- | --- |
| `/observer/sync` | opaque sync token | Compact roster and observer read state. |
| `/agents/{id}/deliveries` | `delivery_seq` | Delivery/read domain. |
| `/agents/{id}/events` | event epoch and seq | Raw replay and catch-up. |
| `/agents/{id}/state` | none | Agent-detail bootstrap snapshot. |
| `/events/stream` | none | Low-latency global wake hint. |

A client may receive a canonical summary saying `latest_event_seq = 900` while
its local raw session is contiguous only through `850`. It should:

- trust sync for unread and roster preview;
- show the conversation as recovering if selected;
- backfill raw events after `850`; and
- mark delivery read only after the corresponding visible delivery has been
  rendered or explicitly acknowledged.

## Retention

### Delivery Metadata

Conversation delivery metadata is compact canonical product state, not ordinary
audit-event evidence. The first implementation does not delete it under
`runtime.retention.audit_events_days`.

If delivery retention is introduced later, it must define:

- the minimum retained prefix per agent incarnation;
- whether unread counts include non-retained deliveries;
- how an old read marker is represented;
- how non-retained marker attempts remain folded into
  `delivery_marker_unavailable`;
- whether a bounded preview survives full brief-content retention; and
- how exact unread semantics remain possible.

The system must not silently turn exact unread into an estimate.

### Observer Change Ledger

The change ledger may be compacted because sync returns current summaries rather
than mutation history.

Compaction must retain a watermark floor. A token older than that floor resets
to a full snapshot with `reset_reason = token_expired`.

Compaction does not change agent incarnations, delivery sequences, read
markers, or raw `event_log_epoch`.

### Read State

Read state remains while both observer scope and agent incarnation remain
valid. Credential revocation or authority removal may retain
encrypted/internal rows for audit or delete them according to security policy,
but a new principal or authority scope must not inherit them accidentally.

Agent deletion closes the incarnation. Recreating the id creates an ordinary
new incarnation whose read-state lower bound is zero for every observer scope.

## Failure And Recovery

### Transaction Failure

If delivery append, summary mutation, read-marker mutation, or observer
invalidation cannot commit atomically, the whole semantic mutation should fail
or enter an explicit durable repair state. The server must not acknowledge a
read-marker write that was not durably stored.

### Projection Divergence

The implementation provides a deterministic rebuild command or startup repair
for:

- agent summary revisions;
- conversation delivery metadata from retained canonical briefs and typed
  brief-created evidence;
- observer invalidation watermarks; and
- unread projections from deliveries plus read markers.

A rebuild rotates the observer sync projection generation and forces
`reset_reason = projection_rebuilt`. It does not invent new delivery sequences
for already mapped briefs.

### Partial Response Failure

The server serializes the response only after the consistent projection is
complete. It does not return a token for a partially projected roster.

The client persists a new token only after it has durably applied the complete
response. A crash between response receipt and cache commit replays the old
token and receives an idempotent delta or snapshot.

## Migration

### Phase 0: Contract And Fixtures

- accept this RFC;
- define Rust/OpenAPI DTOs and machine-readable errors;
- add checked JSON fixtures for snapshot, delta, reset, and read-marker cases;
- define one pure canonical agent summary projection; and
- add storage invariants before exposing routes.

### Phase 1: Durable Delivery And Read State

- add agent incarnation records;
- add per-incarnation delivery sequence allocation;
- add conversation delivery metadata;
- add observer principal derivation;
- add observer read-marker records;
- add observer change sequence and invalidation writes; and
- provide rebuild and divergence diagnostics.

Existing briefs migrate deterministically:

1. group by agent incarnation;
2. order by the corresponding typed `brief_created.event_seq` when available;
3. otherwise order by `(created_at, brief_id)`;
4. assign delivery sequences in that deterministic order;
5. persist the source relation so rerunning migration is idempotent; and
6. before advertising `observer.sync.v1`, persist on every migrated agent
   incarnation its greatest migrated canonical delivery sequence as the
   `cutover_baseline_seq`, using zero when it has no migrated deliveries;
7. initialize every currently materialized observer scope's marker to that
   incarnation baseline, unless an explicitly authorized one-time import
   supplies a higher valid marker;
8. whenever an observer scope first materializes membership after cutover,
   initialize its absent marker to the persisted incarnation baseline rather
   than zero; and
9. commit incarnation baselines, existing visibility membership, initialized
   markers, and capability activation as one cutover generation.

For migrated incarnations, `cutover_baseline_seq` remains the semantic lower
bound even if a marker row is absent during repair or lazy materialization.
No read or sync transaction may expose membership while interpreting that
absence as zero.

Browser-local markers are not silently uploaded. They are untrusted,
device-local, may refer to raw event sequences, and cannot safely identify the
observer principal or agent incarnation.

The deliberate UX is: migrated history is initially considered read, and only
deliveries committed after the server cutover baseline become unread. This
does not claim to reconstruct the exact pre-upgrade device-local boundary; it
provides one deterministic server boundary and avoids manufacturing unread
history. The incarnation baseline remains durable for scopes first
materialized after cutover. It may name a canonical sequence the scope could
not see at cutover; server initialization is permitted to establish that
internal floor, while client marker writes remain limited to currently
authorized deliveries. The baseline and its reason must be observable for
rollout diagnosis.

An optional Web GUI migration action may still offer:

```text
Mark currently visible deliveries read on this server
```

after the first successful sync. It uses the normal monotonic API and clearly
states that older local counters are being retired. It is useful only when new
deliveries arrived between server cutover and the user's first upgraded-client
sync; it does not upload the old browser marker.

### Phase 2: Read APIs

- expose `/observer/sync`;
- expose the read-marker mutation;
- expose delivery metadata paging;
- add OpenAPI and local/unix client parity;
- add auth, visibility, reset, retention, and concurrency tests; and
- keep existing routes unchanged.

### Phase 3: Web GUI Cutover

- cache sync token and observer summaries in IndexedDB;
- use cached summary for immediate roster rendering;
- sync on bootstrap, reconnect, online, and visibility resume;
- use global SSE only as an invalidation hint;
- replace local unread increments with server projections;
- replace local authoritative read-marker writes with the monotonic API;
- present migrated history as read at the documented server cutover baseline;
- preserve current per-agent raw-event recovery for conversation detail; and
- delete obsolete localStorage read-state migration code after one
  compatibility release.

### Phase 4: Follow-up Conversation API

The later semantic conversation API may use `delivery_seq` as a notification
and read boundary, but it should define its own item identity and anchor paging.
It must not reinterpret raw `event_seq` as a semantic conversation cursor.

## Cutover And Compatibility

During one compatibility release:

- old clients continue using `/agents/list`, per-agent state/events, and local
  unread behavior;
- new clients prefer observer sync when the handshake advertises capability
  `observer.sync.v1`;
- the server writes delivery and observer invalidation state regardless of
  whether a new client is connected;
- new clients do not double-count local SSE and server unread;
- read-marker mutation is enabled only after the client has received a current
  agent incarnation; and
- metrics compare local legacy projections to canonical summaries without
  changing user-visible state.

After acceptance:

- remove local unread authority;
- keep `/agents/list` as a compatibility view or make it consume the same
  canonical summary core;
- retain raw event APIs for Debug and recovery; and
- do not remove global SSE unless another wake mechanism replaces it.

## Invariants

The implementation must enforce:

1. one active incarnation per existing agent id;
2. unique immutable `(agent_incarnation, delivery_seq)`;
3. one delivery sequence per canonical user-facing delivery;
4. delivery append and observer invalidation are atomic;
5. read marker never decreases;
6. successful marker acknowledgement never points beyond a committed delivery;
7. unread count equals the authorized delivery-row count after the marker;
8. canonical summary is observer-independent and never contains authorized
   preview or unread state;
9. sync token never crosses observer scope; the independently retained reset
   verifier makes supported runtime, token-format, or projection generation
   mismatches produce an explicit reset snapshot;
10. a returned token covers every summary included in its response;
11. inaccessible agents do not leak through snapshot, delta, tombstone, count,
    or error detail;
12. raw event retention does not change delivery or read-marker identity;
13. agent id reuse does not inherit old deliveries or read state;
14. a tombstone identifies one exact agent incarnation;
15. observer-private invalidations never appear in another scope's delta;
16. visibility removal tombstones come only from prior materialized membership;
17. a durable per-incarnation migration baseline prevents pre-cutover history
    from becoming unread for both existing and later-materialized scopes; and
18. replaying the same sync or marker request is idempotent.

## Acceptance Matrix

### Storage And Projection

- appending two briefs allocates increasing delivery sequences;
- retrying the same brief append does not allocate twice;
- concurrent delivery appends produce unique ordered sequences;
- observer latest delivery matches the greatest authorized delivery ledger row;
- unread projection counts rows, including any allowed sequence gaps;
- agent recreation rotates incarnation and resets observer overlay;
- projection rebuild is deterministic and rotates sync projection generation.

### Sync

- initial sync returns every authorized public agent exactly once;
- empty roster returns a valid snapshot token;
- valid delta returns every changed agent with its latest complete summary;
- multiple mutations of one agent are coalesced without losing final state;
- empty delta is valid and advances or preserves a usable token;
- agent deletion returns a same-scope tombstone;
- delete and same-id recreate returns an old-incarnation tombstone plus the new
  incarnation summary without deleting the replacement;
- visibility removal returns a tombstone only to scopes with prior materialized
  membership;
- one observer's marker update is absent from another observer's delta;
- expired token returns a reset snapshot;
- runtime replacement returns a reset snapshot;
- after database replacement, an old token within the verifier compatibility
  horizon returns `runtime_replaced`;
- a retired payload-format token whose envelope remains verifiable returns
  `token_version_retired`;
- policy generation change for the same stable scope returns a reset snapshot;
- scope mismatch takes precedence over reset classification and returns no
  roster data;
- response state and token watermark come from one read snapshot;
- unix socket and TCP HTTP return equivalent DTOs.

### Read Marker

- first marker write advances from the incarnation's durable read-state lower
  bound, which is zero for an ordinary new incarnation;
- duplicate write is a no-op;
- lower write is a no-op and returns the higher value;
- two concurrent writes store the maximum;
- unavailable delivery sequence is rejected without distinguishing
  beyond-head, hidden, absent, or pruned cases;
- unavailable-sequence failures use the same HTTP status, body shape, and
  response headers, and practical timing-oracle tests cannot reliably classify
  beyond-head, hidden, absent, or pruned cases;
- marker may advance to a visible delivery across intervening unauthorized
  sequence rows;
- later authorization of a delivery below the marker does not make it unread;
- stale incarnation is rejected;
- inaccessible agent is not disclosed;
- successful write appears in the next sync delta;
- marker update does not enqueue, wake, or mutate the agent.

### Offline Web GUI

- client A closes at delivery 10, deliveries 11-13 occur, and restart shows
  exact unread `3` plus delivery 13 preview without raw global replay;
- client A loses global SSE for several minutes and one sync restores the same
  result;
- client B using the same observer principal sees the same server read marker;
- client with a different observer principal starts with independent read
  state;
- cached roster renders immediately but remains visibly stale until sync;
- sync reset replaces cached roster and removes stale agents;
- selected conversation may still recover raw events while roster unread and
  preview remain correct;
- marking read waits for rendered/explicitly acknowledged delivery and survives
  reload;
- migration advertises sync only after baseline commit, shows zero unread for
  pre-cutover history, and counts the first post-baseline delivery as one;
- a scope first materialized after cutover inherits the durable incarnation
  baseline and does not manufacture unread pre-cutover history;

### Authorization And Privacy

- raw control token is never persisted in observer tables or sync tokens;
- token logs contain only bounded fingerprints;
- private/inaccessible agents never appear in deltas or tombstones;
- credential rotation invalidates or changes observer scope as configured;
- local unauthenticated mode uses one documented deployment-local principal;
- canonical summary bytes are identical across observers with access to the
  same agent lifecycle facts;
- observer latest delivery and preview apply the same authorization and
  redaction as brief fetch.

## Observability

Metrics and structured diagnostics should include:

- sync request mode, duration, agent count, reset reason, and response bytes;
- observer change high watermark and retained floor;
- delivery allocation conflicts or idempotent retries;
- read-marker advance/no-op/rejection counts;
- unread projection latency;
- summary projection divergence;
- token scope/version rejection; and
- Web GUI sync age and last reset reason.

Logs must not include full sync tokens, bearer credentials, full brief text, or
private agent identifiers outside the authorized diagnostic boundary.

## Rejected Alternatives

### Use global SSE as the sync log

Rejected because the current stream is live-only and has no durable global
cursor. Its events expose a runtime audit sequence through agent-filtered
views, but that sequence is neither a global observer-state cursor nor a
conversation delivery cursor.

### Backfill every agent's raw events on every startup

Rejected as the roster authority. It is expensive, depends on retained event
history, requires event projection and brief hydration, and still does not
provide shared observer read state.

Per-agent raw backfill remains necessary for selected conversation recovery.

### Use `event_seq` as `delivery_seq`

Rejected because raw audit events and user-facing deliveries are different
domains. Event retention, debug events, filtered events, and future delivery
sources would make the marker ambiguous.

### Store unread count directly

Rejected as the sole authority because concurrent delivery and read mutations
make counters easy to drift. The canonical facts are delivery rows and a
monotonic marker. A cached/materialized count is allowed only if it is
transactionally maintained and rebuildable from those facts.

### Keep read state only in the browser

Rejected because offline recovery and cross-device consistency are impossible,
and clearing browser storage loses the only marker.

### Accept a client-generated observer id

Rejected because it permits state confusion and unauthorized marker access.
Observer identity comes from authentication.

### Put full conversation history in `/observer/sync`

Rejected because roster synchronization should remain compact and bounded.
Conversation history needs separate semantic item and anchor-pagination
contracts.

### Reuse `/agents/list` and add more optional fields

Rejected as the complete solution because list polling has no durable delta
token, no observer-scoped state, no reset contract, and no atomic read-marker
mutation. The canonical summary core may still be shared with that endpoint.

## Proposed Decision

Accept the following contract:

1. add a durable per-agent-incarnation conversation delivery ledger with
   `delivery_seq`;
2. define one canonical compact agent summary plus an observer-scoped latest
   delivery/read overlay, while keeping raw-event head metadata canonical;
3. derive observer identity from authorization and store read markers by
   complete observer scope plus agent incarnation;
4. update read markers by atomic monotonic maximum;
5. expose exact unread count as an observer overlay;
6. add an opaque-token `/api/observer/sync` snapshot/delta endpoint;
7. keep global SSE as a wake hint and raw per-agent events as the diagnostic
   replay surface;
8. require explicit snapshot reset, retention, migration, and authorization
   behavior; and
9. pause server implementation until this RFC is reviewed and accepted.
