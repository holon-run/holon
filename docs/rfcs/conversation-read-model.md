---
title: RFC: Conversation Read Model
date: 2026-09-13
status: draft
---

# RFC: Conversation Read Model

## 1. Summary and review status

Propose a read-only conversation surface for the Web GUI:

**Page by native turn, present results as briefs, load activity by turn.**

The initial history request returns turn summaries, not historical verbose
events. Active turns receive live activity. Opening a brief's execution details
loads the associated turn's activity on demand. Folding, animation, and manual
expansion preferences remain client decisions.

This is a discussion draft, not an implemented API or authorization to implement.
Route names, DTO names, and example limits below are proposals. Section 11 lists
the decisions that still need review. Actual Codex/ChatGPT App folding behavior
has not been verified and is not a prerequisite for this interface design.

Related native contracts:

- [Operator Display Levels and Event Presentation](./operator-display-levels-and-event-presentation.md)
- [Event Stream Interface](./event-stream-interface.md)
- [Turn Model Lineage and Recovery](./turn-model-lineage-and-recovery.md)

## 2. Current structure and problem

The relevant existing layers are:

| Layer | Current responsibility |
| --- | --- |
| `src/types.rs` | Native turn, terminal, brief, and message identities and records |
| `src/storage/mod.rs`, `src/runtime_db/repositories.rs` | Persistent record queries |
| `src/http/events.rs`, `src/http/briefs.rs` | Raw event history/stream and brief reads |
| `web-gui/app/src/runtime/runtime-store.ts`, `agent-session-repository.ts` in the same directory | History loading, stream recovery, and client state |
| Web GUI conversation/timeline reducers | Derive display items and groups from loaded events and records |

The current conversation history/recovery path is built around a raw event
ledger. Hiding historical activity in the DOM does not stop that path from
fetching or hydrating it. Filtering `/events` by `max_level=info` is also not
equivalent to a page of turn summaries.

The existing raw-stream RFC places projection in first-party clients. This
proposal adds a narrow server-side read projection for lightweight conversation
loading. It is an explicit extension to that architecture, not a claim that the
existing raw stream already has these semantics. Raw events remain available
without changing their contract; the TUI need not migrate.

### Goals

- Load recent and older history without fetching verbose transcripts/tool output.
- Provide stable turn, input, brief, and activity identities.
- Connect summary snapshots, detail pages, and live changes without loss.
- Bound pagination, replay, and recovery work.
- Preserve authorization, canonical provenance, and existing diagnostic APIs.

### Non-goals

- Change scheduling, native turn boundaries, WorkItem lifecycle, or delivery.
- Create a second execution log or copy brief bodies into another authority.
- Design submission/cancellation APIs, token streaming, or a general projector framework.
- Expose debug/trace, provider requests, secrets, or hidden reasoning as activity.
- Specify desktop App parity or require its UI layout.

## 3. Resource and lifecycle model

### 3.1 Identity

- A **turn** is a native runtime execution pass identified by `turn_id`.
  It may originate from input, a timer, a task wake, or recovery.
- A **brief** is a canonical result identified by `brief_id`. A turn may have
  zero, one, or multiple briefs.
- An **input** keeps its canonical message identity. A queued input has no
  fabricated turn; several inputs may be associated with one turn.
- An **activity** is a compact verbose item with a stable `activity_id`,
  canonical source references, and an owning `turn_id`.
- A **conversation** here is an agent-scoped read view, not a new persisted
  Session or scheduling entity.

The server resolves ownership from canonical relationships, not adjacent
timestamps, the latest operator message, WorkItem focus, or a raw sequence range.
Cross-turn task relationships do not transfer ownership of earlier activity.
Multiple briefs from one turn share one detail cache and execution group.

### 3.2 Execution, result availability, and attention are separate

A summary must distinguish:

| Dimension | Proposed meaning |
| --- | --- |
| Execution | Whether the turn is active or terminal; a safe typed terminal outcome |
| Result availability | Whether known briefs are resolved, explicitly absent, or unresolved |
| Attention | Safe error/interruption/wait information requiring visibility |
| Detail availability | Whether activity is available, partial, unavailable, or unknown |

`brief_upsert` does not terminate a turn. Agent idle and WorkItem completion
are not substitutes for a turn terminal record. A wait may close the current
turn while a WorkItem or background task remains open.

Native records already carry `TurnTerminalKind` and optional
`TurnNoBriefReason`. The public mapping must handle completed, aborted,
baseline-budget failure, deferred fallback, and provider-recovery outcomes
without treating every terminal turn as successful or every recovery as final
failure of the user's objective. Raw reason strings and checkpoints are not
public summary fields.

Proposed result states are `pending`, `available`, `none`, and `unavailable`:

- `pending`: result association/materialization is not yet resolved.
- `available`: currently associated briefs can be read.
- `none`: canonical facts explicitly establish no brief.
- `unavailable`: result resolution failed or retained data is insufficient;
  return a safe reason and retryability, not a fabricated result.

`available` does not promise that no later brief can be attached. The summary's
brief list remains revisioned. An empty brief list alone does not prove `none`;
a short timeout must not turn missing evidence into a no-result claim.
How legacy ambiguity maps to `pending` versus `unavailable`, and how finalization
settles unresolved results without endless retries, remain review items.

Attention is typed metadata, not a synthetic `BriefRecord`. Failed/interrupted
turns with no brief remain discoverable. Silent background turns do not acquire
fake assistant output.

## 4. Summary snapshot and history pagination

Candidate route, relative to the existing API base:

```http
GET /agents/{agent_id}/conversation?limit=30
GET /agents/{agent_id}/conversation?before=<next_before_cursor>&limit=30
```

Logical response fields:

| Field | Contract |
| --- | --- |
| `schema_version`, `event_log_epoch` | Response contract and source-log identity |
| `turns[]` | One bounded page of turn summaries, returned in chronological order |
| `active_turns[]` | Active summary records for bootstrap/recovery, not activity bodies |
| `pending_inputs[]` | Visible unassigned inputs for bootstrap/recovery |
| `next_before_cursor`, `has_more` | Older-history navigation, counting turns |
| `snapshot_cursor` | Stream coverage boundary for this coherent read |

A turn summary contains stable identity and ordering information, a revision,
necessary visible inputs, safe execution/result/attention metadata, duration
when known, detail availability, and its `briefs[]`. Each brief includes its
canonical identity, body, citations, and attachment metadata. It does not
require transcript hydration to obtain the body and does not inline attachment
files, commands, tool arguments/output, or intermediate assistant text.

All briefs associated with a turn travel with that turn's summary; the history
page never splits them. `limit` counts turns, not briefs, events, or rendered
rows. Turn-count bounds alone do not bound bytes; payload limits and explicit
handling of an oversized single-turn summary must be decided before shipping,
rather than silently truncating results or splitting the turn.

### 4.1 Page membership and order

1. The first request selects the most recent page of native turns. `active_turns`
   may overlap that page; merge by identity/revision, never render duplicates.
2. Older pages use keyset pagination with a stable total order. A candidate key
   is `(turn_index, turn_id)` within the agent; uniqueness/index assumptions
   must be verified against actual storage.
3. The page chain fixes a membership upper bound from the first page. New turns
   do not shift it. A new brief on an old turn updates that summary, not its
   position. No offset pagination or timestamp-only ordering.
4. The proposal freezes page membership, not every historical value across
   requests. Each page is its own coherent read; mutable fields may be newer.
   Clients merge revisions and preserve live state against late page responses.
5. Bootstrap includes `active_turns` and `pending_inputs`; older-page reads need
   not repeat them. Older-page `snapshot_cursor` is not permission to advance an
   already connected client's stream checkpoint past unconsumed changes.

Opaque means clients store and return the cursor unchanged. A pagination cursor
may encode the ordering key, page-chain upper bound, agent, epoch, query version,
and scope. It is **not just a turn index** and is not a stream cursor.

### 4.2 Snapshot/stream consistency

Summary data, active turns, pending inputs, and `snapshot_cursor` must describe
one consistent coverage boundary. Reading rows and then independently taking a
later event-log head can omit changes forever and is prohibited.

Implementation must prove one mechanism: a consistent transaction over source
records and their covered watermark, or a projection barrier tied to committed
source changes. Every visible record mutation must participate in that coverage.
If current writes cannot provide this guarantee, repair that boundary before
exposing the API; do not disguise an arbitrary event head as a snapshot cursor.

Query canonical records and indexes without scanning the entire audit history
per page. Add keyset queries/indexes first. Only introduce a rebuildable reference
index if bounded queries and consistent coverage cannot otherwise be achieved;
it must not become another copy of authoritative result bodies.

### 4.3 Legacy records

Nullable event linkage or missing turn ownership must not silently remove old
briefs or manufacture native turns. Return reliably associated legacy records
normally, marking missing activity coverage explicitly.

For briefs that cannot be attributed to any turn, the compatibility surface is
still open: a separately paginated legacy result collection or an explicit
legacy-history entry point are candidates. The normal turn page must not contain
pseudo-turns. A decision and migration fixtures are required before replacing the
old history UI.

## 5. Single-turn activity read

```http
GET /agents/{agent_id}/turns/{turn_id}/activities?limit=50
GET /agents/{agent_id}/turns/{turn_id}/activities?before=<detail_cursor>&limit=50
```

Return turn identity/metadata, `activities[]`, `next_before_cursor`, `has_more`,
`coverage`, and snapshot/revision metadata. The first page selects the most
recent activity and returns it in chronological order; older activity is loaded
explicitly. `coverage` distinguishes complete history, retained partial history,
and unavailable/unknown history. `has_more=false` alone does not mean complete.

Activity IDs and ordering keys remain stable as tools progress. Each item has
a comparable monotonic revision within its identity and epoch. A tool status
update replaces that item; it is not a second tool invocation. Known final
assistant output and the delivered brief are associated through canonical
finalization references, not text matching.

These are safe typed verbose items with bounded fields and authorized detail
references. Existing inspectors handle large output/diffs. Shared activity
resolution should be reused rather than implementing another set of tool-state
heuristics in a route. Unknown types have explicit compatibility behavior.

Detail pagination freezes membership independently from summary pagination.
Its cursor is bound to agent, turn, epoch, and query version; it cannot resume
the conversation stream. A late detail response cannot overwrite newer streamed
revisions. Detail read errors leave the already loaded brief intact.

Live streaming is not a promise to continuously update expanded *terminal*
details. A relevant later change should invalidate that turn's detail coverage;
an open detail view can re-fetch bounded pages. The precise invalidation message
is part of the stream DTO review.

## 6. Conversation change stream

```http
GET /agents/{agent_id}/conversation/stream?after=<snapshot_cursor>
```

Reuse SSE transport, authorization, and underlying source subscriptions, while
keeping a separate projection contract:

| Existing `/events/stream` | Proposed conversation stream |
| --- | --- |
| Runtime event envelopes | Changes to visible inputs, summaries, and activities |
| Raw `event_seq` recovery | Scoped opaque conversation checkpoint |
| Raw replay contract | Bounded view reconciliation plus live changes |
| Client resolves display relationships | Server supplies canonical ownership and safe typed fields |

This is a read projection over existing records/events, not a second execution
log. Independent URL versus an explicit projection mode remains open; a distinct
contract is required either way.

### 6.1 Proposed message vocabulary

| Message | Purpose |
| --- | --- |
| `operator_upsert` | Insert/update visible input by message ID, including its eventual turn assignment |
| `brief_upsert` | Insert/update a result by brief ID and native turn ID |
| `turn_state` | Revisioned summary/state update, including result/detail availability |
| `activity_upsert` | Revisioned verbose item for a live-observed active turn |
| `checkpoint` | Advance the boundary fully covered by this view |
| `reset_required` | Invalidate recovery and request a fresh bootstrap |

Messages carry scope/epoch and stable entity identities/revisions. Updates for a
turn outside loaded pages must be self-contained or explicitly invalidate that
summary; a bare terminal flag cannot reconstruct missed briefs and inputs.
The exact choice of full-summary upsert versus invalidation/refetch is open.
Input assignment must remove the matching pending representation by identity.

Revisions are source-derived or durably reproducible, not response timestamps.
The stream checkpoint orders covered changes; entity revisions prevent stale
replacement. They have different roles and cannot be substituted for each other.

### 6.2 Live delivery and reconnect

- Live connections receive current-turn activity plus visible input, brief,
  and lifecycle changes. Do not prefetch historical activity.
- On reconnect, choose a coherent recovery boundary. Turns that ended during
  the gap reconcile to their summaries/terminal state instead of replaying
  every intermediate verbose item. Turns still active receive bounded activity
  recovery; missing earlier detail remains explicitly pageable.
- Reconciliation may coalesce mutations. This is view recovery, not exactly-once
  replay of every raw event.
- Only commit a checkpoint after all required summary changes and detail
  recovery/invalidation instructions through that boundary have been delivered
  and applied. A disconnect mid-batch must be safely replayable.
- Proposed SSE `id` values represent safe resumable checkpoints, not raw sequence
  numbers or incomplete reconciliation progress. Define `Last-Event-ID` support
  and precedence relative to `after` before implementation.
- Filtered source events advance coverage via checkpoints; they are not gaps
  to repair using the raw event ledger.
- Expired cursors, epoch replacement, incompatible query versions, or exceeded
  replay budgets require an explicit reset. Never silently skip to the latest
  event or scan an unbounded gap.
- A normal daemon restart does not by itself imply a new log epoch. Slow
  consumers must not block execution; bounded queues/backpressure may force reset.

Malformed or cross-scope cursors are rejected; valid but no-longer-recoverable
cursors return a typed reset condition. Exact HTTP statuses and SSE error
encoding remain part of DTO review. Existing authorization failure semantics
remain separate from recoverable cursor errors.

## 7. How the three reads cooperate

```text
Open / reset
  1. conversation -> recent summaries, active turns, pending inputs, cursor S
  2. stream(after=S) -> reconcile and then follow changes
  3. activities(active turn) -> bounded process history from before opening

Older history
  conversation(before=history cursor) -> older summaries only

Expand a brief
  activities(brief.turn_id) -> shared per-turn detail cache

Live execution
  stream -> activity, input, result, and state updates

Reconnect
  stream(after=last applied checkpoint)
    -> summary reconciliation + bounded active detail recovery
    -> reset_required if recovery is unavailable
```

Steps 2 and 3 may overlap. Merge by identity and revision. If a turn ends between
steps 1 and 3, the late activity page must not resurrect its active state.
A brief arriving before terminal state does not fold the active turn; terminal
state arriving before brief readiness does not create a blank result.

Maintain separate summary membership, detail pagination/coverage, conversation
checkpoint, and diagnostic raw-ledger state. Cache keys include remote, agent,
epoch, schema version, and entity identity; stale responses from a previous
agent/remote generation must be discarded.

Summary updates outside loaded pages do not falsely mark intervening history
as loaded. Reset clears stale coverage assumptions; it does not mark unread
briefs as read. Existing unread/delivery semantics remain canonical, and detail
expansion alone does not create a new unread result.

## 8. Relationship to the northbound compatibility proposal

Repository issue **holon-run/holon#2904**, "OpenAI Agents API" compatibility,
proposes a broader external surface. This section compares that proposal;
it does not independently validate its claimed external beta API contract.

**Share native facts and bounded query primitives; separate public projections
and delivery.** Neither external surface should call the other.

Share:

- Reliable turn/input/brief/tool/task ownership resolution.
- Terminal, wait, and result-availability facts.
- Bounded identity/keyset reads, snapshot barriers, epoch/reset primitives.
- Source subscriptions and applicable authorization primitives.

Keep separate:

- GUI summary/activity DTOs versus protocol Sessions, Turns, and Items.
- Agent-scoped GUI cursors versus session-scoped protocol sequences.
- GUI compressed recovery versus the protocol's promised event replay.
- Protocol ID persistence, authentication, ownership, idempotency, required
  actions, tool-result submission, environments, and outbound webhooks.

A compatibility Turn around accepted input is not automatically one native
execution turn: recovery or waiting may create additional native turns.
The adapter must explicitly preserve that mapping. Briefs are only part of
protocol output, not a replacement for all protocol Items.

GUI operator permissions must never be inherited by a remote API principal.
Runtime/domain storage must not depend on compatibility schemas. Develop the
narrow GUI reads as needed; #2904 can reuse them without blocking on the entire
GUI or forcing a shared external Session model.

## 9. Compatibility, rollout, and security

- Preserve `/briefs`, `/events`, `/events/stream`, and existing diagnostic reads.
- Advertise the new read capability/version. A new GUI on an old daemon should
  explicitly retain the old mode, not secretly fetch all history and call it
  lightweight mode.
- Ordinary conversation startup must not also initialize old raw-history
  catch-up/transcript hydration through roster or unread recovery side paths.
- Keep debug/trace separate. Reuse verbose visibility rules, not arbitrary raw
  payload serialization.
- Enforce agent and artifact/workspace access at every read. Cursors are not
  capabilities; validate scope and permissions independently.
- Do not persist folding state in runtime records or duplicate brief text.

After review, sequence implementation as: canonical query/coverage proof and
DTOs; backend contract tests; isolated GUI read/cache path; then UI integration
and migration. No implementation is included in this RFC.

## 10. Acceptance evidence required for implementation

1. Initial/older requests contain no historical verbose/transcript/tool output.
   A multi-brief turn stays together; zero-brief failures remain visible.
2. Concurrent insertions and late briefs do not duplicate/skip page membership
   or reorder old turns. Page cursors cannot be reused across agents/epochs.
3. A change committed between snapshot construction and stream attachment is
   observed; replay expiry produces reset rather than silent loss.
4. Both terminal-before-brief and brief-before-terminal work; waits, recovery,
   aborts, missing records, and unresolved result finalization have fixtures.
5. Detail snapshots racing stream updates converge by identity/revision,
   including tool completion and a turn ending during bootstrap.
6. Reconnect over completed turns returns summaries, not their entire process;
   active detail recovery is bounded. Mid-reconciliation disconnect is safe.
7. Long turns paginate; retention gaps differ from complete/empty detail.
   Unattributable legacy briefs stay accessible without invented turn IDs.
8. Query plans demonstrate indexed bounded reads, not full audit-log scans.
   Oversized summary behavior and slow-consumer recovery are tested.
9. Agent/remote switches discard stale responses; authorization and artifact
   access checks cover the new paths. Raw consumers remain compatible.
10. Network-level GUI tests prove hidden legacy hydration/recovery paths are not
    fetching verbose history. Existing unread behavior and manual expansion
    preferences remain intact.

## 11. Review agenda

| Decision | Current recommendation / unresolved part |
| --- | --- |
| Pagination unit | Native turn; briefs remain grouped and detail is per turn |
| Endpoint organization | Three reads as above; new stream URL versus explicit mode remains open |
| Summary DTO and size | Include readable briefs/inputs; settle exact safe fields and oversized-turn behavior |
| Ordering and cursor encoding | Keyset + fixed membership upper bound; validate storage key, expiry, scope/version rules |
| Consistent coverage | Prove source writes and snapshot watermark share a boundary before choosing implementation |
| Public terminal/result mapping | Keep execution/result/attention separate; settle recovery outcomes and unresolved/legacy finalization |
| Stream reconciliation | Coalesced completed summaries, bounded active recovery; settle full upserts/invalidation and checkpoint framing |
| Legacy ownership | No invented turns or dropped briefs; choose a bounded compatibility entry point |
| Detail invalidation | Explicitly signal late terminal-detail changes; finalize message and refresh semantics |
| Relationship to #2904 | Reuse native facts; independent external identities, DTOs, permissions, and replay contracts |

Discuss and revise this document before freezing OpenAPI/TypeScript types.
Approval of the RFC and authorization to implement are separate steps.
