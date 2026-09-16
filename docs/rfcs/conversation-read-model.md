---
title: RFC: Conversation Read Model
date: 2026-09-13
status: accepted
---

# RFC: Conversation Read Model

## 1. Summary and status

Propose a read-only conversation surface for the Web GUI:

**Page by native turn, present results as briefs, load activity by turn.**

The initial history request returns turn summaries, not historical verbose
events. Active turns receive live activity. Opening a brief's execution details
loads the associated turn's activity on demand. Folding, animation, and manual
expansion preferences remain client decisions.

This RFC was accepted for phased implementation on 2026-09-13. Sections 3-7
define the normative v1 contract. Concrete DTO field names and hard limit values
may be refined without changing the lifecycle, consistency, or compatibility
boundaries frozen here.

The initial server implementation did not modify the existing `web-gui`. The summary and
detail reads and the conversation change stream are implemented by the Rust
runtime. An independent Web/TypeScript protocol SDK remains available for a
later phase and GUI integration WorkItem. Actual Codex/ChatGPT App folding
behavior has not been verified and is not a prerequisite for this interface.

As of 2026-09-14, the v1 server and independent SDK implementation also include
the compatibility and observability phase: fail-closed control/schema/query
version checks, bounded metadata-only shadow diagnostics, label-free
conversation metrics, and recovery fixes for long active turns, live terminal
summaries, and legacy ownership/result mappings. The existing `web-gui` remains
unchanged and no second event ledger has been introduced.

As of 2026-09-15, the Web GUI consumes the SDK turn read model. Its reading,
folding, and scrolling behavior is documented in [the GUI design contract](../../web-gui/DESIGN.md#conversation-reading-contract).
The display-summary refinement below keeps the existing v1 wire shape.

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

Mutable public entities use durable monotonic revisions distinct from the
stream checkpoint. Prefer the canonical audit `event_seq` allocated in the same
transaction as the visible mutation. If a source cannot provide that atomic
linkage, add an entity-local monotonic revision before advertising the
capability. Timestamps are never revisions. Immutable Brief content starts at
revision 1; a turn-summary revision covers input assignment, Brief membership,
execution/result/finality, attention, and detail-coverage changes.

`pending_inputs` is a projection, not another lifecycle. It contains visible
operator inputs whose canonical queue/assignment state is `queued` or
`assigning` and which may still form a future turn. Assigned, processed,
interjected, aborted, dropped, and quarantined inputs are not pending. The
dequeue-to-turn-assignment transition must be atomic or revisioned so an input
cannot disappear between the pending set and its owning turn.

Each pending input carries a bounded message-body preview with the same shape
and limits as assigned turn-input previews, so clients can echo the operator's
in-flight text while the owning turn is still running.

### 3.2 Execution, result availability, and attention are separate

A summary must distinguish:

| Dimension | Proposed meaning |
| --- | --- |
| Execution | Whether the turn is active or terminal; a safe typed terminal outcome |
| Input previews | Canonical message ids plus bounded text previews for inputs assigned to the turn |
| Result availability | Whether known briefs are resolved, explicitly absent, or durably unavailable |
| Result finality | Whether canonical delivery lifecycle proves the result set is settled |
| Attention | Safe error/interruption/wait information requiring visibility |
| Detail availability | Whether activity is available, partial, unavailable, or unknown |

`inputs` on a turn summary carries the assigned input message ids with bounded
message-body previews (at most 8 per turn). This lets history pages render
inputs without issuing per-turn activity detail requests; the authoritative
activity sequence remains in turn detail. Input assignment bumps the turn
summary revision, so stream consumers receive refreshed previews on the same
revision path.

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
- `unavailable`: durable canonical linkage, retention, or legacy coverage is
  insufficient to resolve the result; return a safe typed reason and
  retryability, not a fabricated result.

Every summary also carries `settled`. It is true only when canonical delivery
lifecycle durably proves that no later Brief can join the result set. Turn
terminal state, agent idle state, a timeout, or the current contents of
`briefs[]` do not imply `settled`. `available` may therefore be either settled
or unsettled, and the summary's Brief list remains revisioned.

Transient storage, decoding, or authorization failures fail the HTTP/stream
operation. They never map to `unavailable`. An empty Brief list alone does not
prove `none`, and a short timeout must not turn missing evidence into a
no-result claim.

Attention is typed metadata, not a synthetic `BriefRecord`. Failed/interrupted
turns with no brief remain discoverable. Silent background turns do not acquire
fake assistant output.

### 3.3 Canonical source and event-coverage inventory

Phase 0 records the current storage boundary below. `verified` means the
existing write already provides the required committed linkage. `blocked`
means the source remains readable for diagnostics but prevents
`agents.conversation-read.v1` advertisement.

| Source | Canonical write boundary | Phase 0 state |
| --- | --- | --- |
| Turn create/update | `TurnRecord` repository upsert; terminal transitions use `commit_turn_terminal` | Blocked: ordinary create/update lacks one projection revision/event linkage |
| Input and assignment | message evidence plus queue/turn transitions | Blocked: pending-to-turn assignment is not yet one proven atomic transition |
| Brief result | `append_brief_with_created_event` | Verified: Brief, `brief_created`, `event_seq`, and `created_event_seq` commit together |
| Assistant activity | transcript/assistant-round evidence | Blocked: no atomic created-event linkage or unified revision |
| Tool activity | tool-execution evidence plus `tool_executed` audit event | Blocked: ordinary record and event currently commit separately |
| Wait/error activity | wait/queue/task transitions and typed audit events | Blocked: transition and direct write paths do not have complete shared coverage |
| Result finality | canonical delivery/terminal lifecycle | Blocked: `settled` is not yet a revisioned read-model fact |
| Snapshot watermark | observer projection snapshot read transaction and per-Agent event head | Verified foundation; conversation-specific sources still need coverage |

Phase 1 replaced these blockers with durable source revisions, canonical turn
ownership/assignment linkage, atomic source-event coverage, and a recomputed
`conversation_read_verified` diagnostic. The capability now identifies binary
protocol support: once schema migration succeeds, the runtime advertises and
serves `agents.conversation-read.v1` even when historical diagnostic checks
report projection drift. Migration repairs proven assignment drift, while
individual reads retain typed failures instead of disabling the whole node.

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
| `pending_inputs[]` | Visible unassigned inputs for bootstrap/recovery, each with the same bounded message-body preview shape as assigned turn inputs |
| `next_before_cursor`, `has_more` | Older-history navigation, counting turns |
| `snapshot_cursor` | Stream coverage boundary for this coherent read |

A turn summary contains stable identity and ordering information, a revision,
necessary visible inputs, safe execution/result/attention metadata, duration
when known, detail availability, and its `briefs[]`. Each brief includes its
canonical identity, citations, safe attachment metadata, and
`content_state = inline | deferred`. Inline content does not require transcript
hydration. Deferred content preserves complete Brief membership and is fetched
through the existing authorized Brief read. A summary never inlines attachment
files, arbitrary attachment values or URIs, commands, tool arguments/output, or
intermediate assistant text.

Phase 2 applies the RFC's allowed concrete-DTO refinement by publishing complete
Brief membership as canonical `brief_ids[]`. Brief bodies, citations,
attachments, artifact values, workspace metadata, and URIs remain on the
existing authorized Brief read surface and are never copied into the
conversation snapshot. Clients hydrate selected Brief IDs through that surface;
legacy Briefs without a canonical turn remain reachable there but are not
attached to a pseudo-turn.

All briefs associated with a turn travel with that turn's summary; the history
page never splits them. `limit` counts turns, not briefs, events, or rendered
rows. Count and byte budgets are both mandatory. An oversized Brief body becomes
`deferred`; an oversized summary returns a typed error that preserves its cursor
position rather than silently truncating, splitting the turn, or permanently
blocking pagination.

### 4.1 Page membership and order

1. Every native turn belongs to the conversation page chain, including timer,
   task-wake, recovery, operational, and otherwise silent turns. Each turn gets
   an immutable `presentation_class` derived from its creation trigger.
   Presentation clients may fold classes but must not change API membership.
2. The first request selects the most recent page of native turns. `active_turns`
   may overlap that page; merge by identity/revision, never render duplicates.
3. Older pages use keyset pagination with a stable total order. A candidate key
   is `(turn_index, turn_id)` within the agent; uniqueness/index assumptions
   must be verified against actual storage.
4. The page chain fixes a membership upper bound from the first page. New turns
   do not shift it. A new brief on an old turn updates that summary, not its
   position. No offset pagination or timestamp-only ordering.
5. The contract freezes page membership, not every historical value across
   requests. Each page is its own coherent read; mutable fields may be newer.
   Clients merge revisions and preserve live state against late page responses.
6. Bootstrap includes `active_turns` and `pending_inputs`; older-page reads need
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

The v1 capability is `agents.conversation-read.v1`. It remains absent unless a
durable verifier proves the required source tables, canonical ownership
linkages, monotonic entity revisions, and source-event coverage for turn,
message assignment, Brief, assistant activity, tool activity, wait/error, and
detail invalidation mutations. Route registration alone never advertises the
capability.

Query canonical records and indexes without scanning the entire audit history
per page. Add keyset queries/indexes first. Only introduce a rebuildable reference
index if bounded queries and consistent coverage cannot otherwise be achieved;
it must not become another copy of authoritative result bodies.

### 4.3 Legacy records

Nullable event linkage or missing turn ownership must not silently remove old
briefs or manufacture native turns. Return reliably associated legacy records
normally, marking missing activity coverage explicitly.

Briefs that cannot be attributed to any turn remain accessible through the
existing bounded `/briefs` compatibility surface. The normal turn page never
contains pseudo-turns, and v1 does not introduce another authoritative legacy
collection. Migration fixtures must prove that legacy results remain reachable.

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

The v1 activity vocabulary is closed:

| Activity | Stable identity |
| --- | --- |
| `operator` | canonical `message_id` |
| `assistant` | canonical transcript/assistant-round identity |
| `tool` | canonical `tool_execution_id` |
| `wait` | canonical wait record or typed audit-event identity |
| `error` | canonical typed failure/event identity |

Briefs are results and are not duplicated as activity. Unknown source types are
omitted and force `coverage = partial` with a safe typed reason. V1 does not
publish a generic item that can carry arbitrary source payload.

These are safe typed verbose items with bounded fields and authorized detail
references. Existing inspectors handle large output/diffs. Shared activity
resolution should be reused rather than implementing another set of tool-state
heuristics in a route. Unknown types have explicit compatibility behavior.

### Publishing active execution

The runtime persists the native turn record when execution begins, before the
provider request, then publishes `turn_started`. Input assignment and the turn
revision therefore become readable while the provider is still running. The
terminal transition updates that existing identity; run, owner, trigger,
creation time, and replay provenance remain fixed at admission. A failed terminal
settlement retains the active admission record without inventing a terminal.
Prompt history excludes the active turn carrying the current input, which is
already represented by the current-input and in-flight round sections.

Expanded clients refresh invalidated detail pages, including active turns whose
activity set exceeds the bounded stream budget. They do not wait for a Brief or
reconstruct a second turn ledger from generic audit events.
Transcript mutations advance both detail and summary revisions: transcript kinds
also determine the summary's detail coverage. Reusing a summary revision after
that coverage changes violates the SDK's immutable-revision contract and stops
live delivery.

### Activity display summaries

`summary` remains a bounded display field, not a serialized provider envelope.
For assistant activity, extract only text blocks from the canonical transcript
before taking the first 4,000 characters. Thinking blocks, signatures, tool-call
arguments, and provider checkpoint state are excluded. An activity without
visible text has an empty summary; clients may label that absence. Legacy
text-only transcript entries may use their string `text` field.

Tool summaries include the canonical tool name (up to 128 characters) and
status. Full tool output and its summary stay in the object inspector. Typed errors use a
short failure label; their evidence remains in authorized detail inspection.
Operator previews retain the existing MessageBody preview representation.
This refinement does not add fields, change identities, or alter pagination and
stream revisions. Extraction runs inside the existing indexed page query.

Detail pagination freezes membership independently from summary pagination.
Its cursor is bound to agent, turn, epoch, and query version; it cannot resume
the conversation stream. A late detail response cannot overwrite newer streamed
revisions. Detail read errors leave the already loaded brief intact.

Live streaming does not continuously update expanded *terminal* details. A
relevant later change emits `detail_invalidated` with the owning turn and detail
revision; an open detail view re-fetches bounded pages. Only active turns may
receive activity deltas, and their recovery count/bytes are hard bounded.

### 5.1 Phase 2 HTTP bounds and typed failures

The initial implementation uses these hard bounds:

- history page: default 30 turns, maximum 100;
- activity page: default 50 items, maximum 200;
- one serialized turn summary: 64 KiB;
- one serialized activity item: 256 KiB;
- complete summary response: 2 MiB;
- complete activity response: 4 MiB;
- one database-read and assembly attempt: 10 seconds.

Invalid requested counts return `400 conversation_invalid_limit`. Count budgets
enforced while assembling active turns, pending inputs, or Brief membership
return `413 conversation_count_limit_exceeded`. Item and response byte budgets
return stable `413` codes identifying the oversized resource. A read timeout
returns retryable `503 conversation_snapshot_timeout`; cursor scope/version/
integrity failures and fixed-coverage violations remain typed rather than
falling back to an unbounded or silently truncated response.

## 6. Conversation change stream

```http
GET /agents/{agent_id}/conversation/stream?after=<snapshot_cursor>
```

Reuse SSE transport, authorization, and underlying source subscriptions, while
keeping a separate projection contract:

| Existing `/events/stream` | Conversation stream |
| --- | --- |
| Runtime event envelopes | Changes to visible inputs, summaries, and activities |
| Raw `event_seq` recovery | Scoped opaque conversation checkpoint |
| Raw replay contract | Bounded view reconciliation plus live changes |
| Client resolves display relationships | Server supplies canonical ownership and safe typed fields |

This is a read projection over existing records/events, not a second execution
log. V1 uses the independent
`GET /agents/{agent_id}/conversation/stream` endpoint and leaves the raw stream
contract unchanged.

### 6.1 Message vocabulary

| Message | Purpose |
| --- | --- |
| `batch_begin` | Start one bounded reconciliation/live batch identified by `batch_id` |
| `operator_upsert` | Insert/update a visible pending input by canonical message identity |
| `operator_remove` | Remove an input from pending by identity after assignment or terminal queue state |
| `turn_summary_upsert` | Lightweight full summary, including Brief membership and entity revision |
| `activity_upsert` | Revisioned verbose item for a live-observed active turn |
| `detail_invalidated` | Advance a turn's detail revision and require bounded refetch |
| `checkpoint` | End the batch and expose its resumable opaque cursor |
| `reset_required` | Invalidate recovery and request a fresh bootstrap |

Messages carry scope/epoch and stable entity identities/revisions. Updates for a
turn outside loaded pages use a lightweight full-summary upsert; a bare terminal
flag cannot reconstruct missed Briefs and inputs. Clients retain it only when
the turn is already loaded or belongs to the bounded live window, so the stream
does not require an unbounded local history cache. Input assignment removes the
matching pending representation by identity.

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
- The server emits `checkpoint` only after every required change through that
  boundary has been encoded and sent in the same bounded batch. The client
  buffers the batch, applies it atomically, and only then persists the
  checkpoint/SSE id. The server never claims to know that the client applied it.
  A disconnect before `checkpoint` replays from the client's previously
  persisted cursor.
- SSE `id` values represent safe resumable checkpoints, not raw sequence
  numbers or incomplete reconciliation progress. `Last-Event-ID` takes
  precedence over `after`; `after` is used only when the header is absent.
- Filtered source events advance coverage via checkpoints; they are not gaps
  to repair using the raw event ledger.
- Expired cursors, epoch replacement, incompatible query versions, or exceeded
  replay budgets require an explicit reset. Never silently skip to the latest
  event or scan an unbounded gap.
- A normal daemon restart does not by itself imply a new log epoch. Slow
  consumers must not block execution; bounded queues/backpressure may force reset.

Malformed or cross-scope cursors are rejected; valid but no-longer-recoverable
cursors return a typed reset condition. Before SSE attachment, malformed or
cross-scope cursors return a normal HTTP error, while retention, epoch, schema,
query-version, cursor-ahead, and replay-budget failures return a typed
`conversation_reset_required` response. After attachment, a live recovery
failure emits `reset_required` when the bounded sender can still accept it and
then closes the stream. Existing authorization failure semantics remain
separate from recoverable cursor errors.

The v1 query accepts the opaque `after` cursor plus bounded `limit` and
`activity_limit` recovery budgets. Each emitted batch starts with `batch_begin`;
only its terminal `checkpoint` carries an SSE `id`. The implementation uses a
bounded per-connection queue and bounded send timeout, so a slow or disconnected
consumer cannot block canonical event writers.

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
- Advertise `agents.conversation-read.v1` whenever the running binary supports
  the migrated contract. `conversation_read_verified` remains diagnostic and
  must not act as a node-wide feature flag.
- Ordinary conversation startup must not also initialize old raw-history
  catch-up/transcript hydration through roster or unread recovery side paths.
- Keep debug/trace separate. Reuse verbose visibility rules, not arbitrary raw
  payload serialization.
- Enforce agent and artifact/workspace access at every read. Cursors are not
  capabilities; validate scope and permissions independently.
- Do not persist folding state in runtime records or duplicate brief text.

The v1 compatibility matrix is:

| Boundary | Required value | Incompatible behavior |
| --- | --- | --- |
| Control handshake | `holon-control` protocol version `1` | SDK fails closed before conversation route use |
| Capability | `agents.conversation-read.v1` | Treat the surface as unavailable; do not probe routes |
| Summary/activity/stream | schema version `1`, query version `1` | Typed decode/reset failure; bootstrap or upgrade instead of guessing |
| Cursor/checkpoint | Opaque and scope/epoch/version bound | Typed cursor/reset response; never reinterpret client-side |

Rollout starts with capability preflight and a legacy client fallback. Operators
then compare a bounded recent window through the control-authenticated
`/api/control/agents/{agent_id}/conversation/shadow-diagnostics` endpoint and
watch the label-free conversation group in
`/api/control/runtime/performance` or `/api/control/runtime/metrics`.
Shadow reports contain IDs, revisions, membership/coverage counters, and
bounded mismatch samples only; they never contain Brief bodies, transcript
text, or tool payloads. The detailed rollout and rollback procedure is in
`docs/conversation-read-model-rollout.md`.

Sequence implementation as: source/event coverage proof; revisions and bounded
queries; snapshot/detail DTOs and backend contract tests; stream/recovery;
independent Web/TypeScript protocol SDK and real protocol E2E; compatibility
and observability. Existing `web-gui` repository/cache, dual-mode integration,
UI migration, and legacy side-path removal are explicitly out of scope.

### 9.1 Web GUI cutover (2026-09 follow-up)

The GUI migration originally deferred above has since landed on
`feat/web-gui-conversation-read-model`: normal conversation pages render only
from the SDK read model (`snapshot → stream → summary/brief/detail` with
opaque-cursor history paging), and the legacy side paths were deleted rather
than left as a fallback:

- Removed: the raw-event virtualized timeline in `AgentPage`, the
  `AgentTimeline`/`timeline-utils` projection, `ensureAgentSession` /
  `catchUpEvents` / `loadTargetEventWindow` / message-transcript-brief batch
  hydration, `loadOlderAgentEvents` semantic-history paging, the per-agent
  legacy session-content cache read/write, and resume-time full-session
  hydration. Ordinary conversation startup no longer triggers raw `/events`
  paging or transcript hydration through roster, unread, or resume paths.
- Kept intentionally: the raw event projection stack that backs the
  independent Debug `timeline-events` view and diagnostics bridge, the durable
  event ledger (unread markers, recovery, truncation acknowledgement), roster
  and run-state sync, and the model-catalog cache. The legacy per-agent
  session-content records are cleared per remote on init instead of being
  imported; connection config, read markers, and diagnostics ledger storage
  are untouched.

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
10. The independent TypeScript SDK runs the same bootstrap, history, detail,
    deferred-Brief, reconnect, reset, retention, and backpressure scenarios
    against a real HTTP/SSE server. Network assertions prove it never fetches
    historical `/events` or transcript/tool hydration.
11. The SDK has no `web-gui` store/view-model dependency and the existing
    `web-gui` is unchanged by this implementation.
12. Compatibility tests reject unknown control/schema/query versions and
    capability absence. Shadow diagnostics and performance/OpenMetrics
    snapshots expose only bounded metadata and fixed, label-free series.

## 11. Accepted v1 decisions

| Decision | Frozen v1 contract |
| --- | --- |
| Pagination membership | Every native turn; immutable trigger-derived `presentation_class` |
| Endpoint organization | Summary page, per-turn detail, and distinct conversation SSE stream |
| Summary DTO and size | Complete Brief membership; bounded inline content with `deferred` body fallback |
| Ordering and cursor encoding | Keyset + fixed membership upper bound; opaque agent/epoch/scope/schema/query-bound cursor |
| Consistent coverage | Same committed source view and covered watermark; capability gated on durable verification |
| Public terminal/result mapping | Execution, result, `settled`, attention, and detail coverage remain separate |
| Entity revisions | Same-transaction canonical event sequence where possible, otherwise durable monotonic entity revision |
| Pending inputs | Only `queued | assigning`; assignment atomically/revisionedly moves identity to a turn |
| Activity vocabulary | Closed operator/assistant/tool/wait/error set; unknown kinds omitted with partial coverage |
| Stream reconciliation | Lightweight full-summary upsert, bounded active recovery, terminal detail invalidation |
| Checkpoint framing | Server completes a batch; client atomically applies then persists; `Last-Event-ID` wins |
| Legacy ownership | Existing bounded `/briefs`; no invented turn or new authoritative legacy collection |
| Relationship to #2904 | Reuse native facts; independent external identities, DTOs, permissions, and replay contracts |

Changes to these lifecycle or consistency decisions require an RFC amendment.
DTO spelling, documented hard limits, and additive safe fields may evolve inside
the versioned capability and schema process.
