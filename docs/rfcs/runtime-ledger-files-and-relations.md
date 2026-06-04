---
title: RFC: Runtime Ledger Files and Relations
date: 2026-06-04
status: draft
Handle: rfc-runtime-ledger-files-and-relations
---

# RFC: Runtime Ledger Files and Relations

## Summary

Holon should make the `.holon/ledger/*.jsonl` files explicit as a small set of
domain ledgers with clear source-of-truth boundaries, causal references, and
state/evidence responsibilities.

The current implementation already persists most runtime state as append-only
JSONL records, but the files are easier to add than to reason about. This RFC
defines the intended role of each ledger, the relations between them, and the
direction for a future refactor.

The main design boundary is:

```text
messages      = authority-bearing admitted inputs
queue         = admission and scheduling state
turns         = causal execution spine
tools         = side-effect evidence
briefs        = outcome/status summaries, not raw input
transcript    = model-facing conversation trace
events        = audit/debug/event-stream mirror
lifecycle     = latest object state reconstructed from append histories
context       = memory, episode, workspace, and identity state
```

The longer-term storage direction is stricter but not event-sourced by
default:

```text
state_tables = canonical current Holon-owned lifecycle state
evidence     = immutable runtime evidence and traces
audit_events = cursorable UI/SSE/debug mirror
```

In that target model, append-only lifecycle snapshots should stop acting as both
log and state. Holon-owned state should live directly in database state tables,
while evidence and audit records remain separate immutable or append-only
records.

## Related documents

This RFC consolidates concerns currently spread across:

- [Runtime Ledger Sequences and Object Revisions](./runtime-ledger-sequences-and-revisions.md)
- [Turn-Based Context Projection](./turn-based-context-projection.md)
- [Turn Model Lineage and Recovery](./turn-model-lineage-and-recovery.md)
- [Turn-Local Context Compaction](./turn-local-context-compaction.md)
- [Long-Lived Context Memory](./long-lived-context-memory.md)
- [Work Item Runtime Model](./work-item-runtime-model.md)
- [Scheduler Wait State](./scheduler-wait-state.md)
- [External Trigger Capability](./external-trigger-capability.md)
- [Operator Display Levels and Event Presentation](./operator-display-levels-and-event-presentation.md)
- [Recent Turns Context Spine](../rfc-implementation-notes/recent-turns-context-spine.md)

Those documents define individual mechanisms. This RFC defines the ledger
inventory and relationship model that should hold them together.

## Problem

Holon currently has many append-only JSONL files under `.holon/ledger/`.
They are useful, but their roles are not consistently documented:

- some files represent canonical authority-bearing inputs
- some represent mutable lifecycle object histories
- some represent model-facing traces
- some represent audit or delivery side effects
- some are projections or summaries used by prompt construction
- some are runtime coordination records used by scheduler recovery

Without an explicit relationship model, design discussions repeatedly run into
the same questions:

- Is a record a source of truth or a projection?
- Should an operator input become a brief, a message, a transcript entry, or all
  of them?
- Should an acknowledgement be a brief, a queue event, or a turn lifecycle
  status?
- Should prompt context read from `turns.jsonl`, `briefs.jsonl`,
  `messages.jsonl`, or independent recent windows?
- Which ledger owns ordering: JSONL append order, `*_seq`, `turn_index`,
  `revision`, or timestamps?
- Which ledgers are safe to merge, migrate, or delete?
- Which records must preserve trust, provenance, and authority boundaries?

This ambiguity makes incremental runtime work harder and makes a broad ledger
refactor risky.

## Goals

- Define the current ledger inventory.
- Classify each ledger as canonical input, causal spine, lifecycle history,
  side-effect evidence, delivery record, prompt/context support, or audit stream.
- Define source-of-truth boundaries between message, queue, turn, transcript,
  brief, tool, event, task, wait, work item, memory, workspace, and identity
  records.
- Define how records should reference one another.
- Clarify prompt projection boundaries, especially the role of `recent_turns`.
- Clarify that operator input is not a brief.
- Clarify that ordinary `Ack` records are admission/lifecycle evidence, not task
  outcomes.
- Document the database-backed state storage direction so database migration
  does not merely copy the current JSONL ambiguity into SQLite.
- Provide a staged refactor direction that keeps historical ledgers readable.

## Non-goals

- Do not define the final schema for every record type.
- Do not require an immediate migration of historical JSONL files.
- Do not require immediate replacement of every JSONL ledger with a database
  table.
- Do not make `events.jsonl` the canonical source for all domain state.
- Do not require all files to gain sequence fields immediately.
- Do not define a distributed cross-agent ordering model.
- Do not define UI presentation logs outside `.holon/ledger/`.

## Current ledger inventory

The current runtime storage creates the following files under
`.holon/ledger/`.

| File | Current record type | Role |
| --- | --- | --- |
| `messages.jsonl` | `MessageEnvelope` | Canonical admitted runtime messages and authority-bearing inputs. |
| `queue_entries.jsonl` | `QueueEntryRecord` | Admission and scheduling lifecycle for messages. |
| `turns.jsonl` | `TurnRecord` | Lightweight causal spine for one runtime activation. |
| `transcript.jsonl` | `TranscriptEntry` | Model-facing conversation trace and provider round history. |
| `tools.jsonl` | `ToolExecutionRecord` | Tool invocation, side-effect, and verification evidence. |
| `briefs.jsonl` | `BriefRecord` | User/model-visible outcome or status summaries. |
| `delivery_summaries.jsonl` | `DeliverySummaryRecord` | Delivery closure records for result/completion reporting. |
| `events.jsonl` | `AuditEvent` | Audit/event-stream/debug mirror with event cursor semantics. |
| `tasks.jsonl` | `TaskRecord` | Managed task lifecycle history. |
| `work_items.jsonl` | `WorkItemRecord` | Work item lifecycle history and latest resumable objective state. |
| `work_item_delegations.jsonl` | `WorkItemDelegationRecord` | Parent/child work delegation lifecycle. |
| `timers.jsonl` | `TimerRecord` | Timer lifecycle state and fire counters. |
| `waiting_intents.jsonl` | `WaitingIntentRecord` | Historical wait intent records. |
| `wait_conditions.jsonl` | `WaitConditionRecord` | Scheduler-visible wait conditions and wake state. |
| `external_triggers.jsonl` | `ExternalTriggerRecord` | External wake capability descriptors. |
| `operator_notifications.jsonl` | `OperatorNotificationRecord` | Operator-facing notification lifecycle. |
| `operator_transport_bindings.jsonl` | `OperatorTransportBinding` | Operator transport binding state. |
| `operator_delivery_records.jsonl` | `OperatorDeliveryRecord` | Per-surface operator delivery attempts/results. |
| `working_memory_deltas.jsonl` | `WorkingMemoryDelta` | Append-only memory state deltas. |
| `context_episodes.jsonl` | `ContextEpisodeRecord` | Compacted episode/context summaries and recovery anchors. |
| `workspaces.jsonl` | `WorkspaceEntry` | Workspace registry state. |
| `workspace_occupancies.jsonl` | `WorkspaceOccupancyRecord` | Active workspace occupancy history. |
| `agent_identities.jsonl` | `AgentIdentityRecord` | Agent identity/profile history. |

## Current implementation usage

The current implementation centralizes ledger paths and basic append/read
helpers in `src/storage.rs`. Higher-level runtime modules decide when each
domain record is created and how recent or latest-state views are reconstructed.

This section describes the current implementation, not the final desired
contract. It should be kept close to the code while the ledger refactor is in
progress.

| File | Written by | Read by / used for |
| --- | --- | --- |
| `messages.jsonl` | `Runtime::enqueue` appends every admitted `MessageEnvelope` and assigns `message_seq`. Tests and recovery helpers may append fixtures. | Prompt context reads recent and all messages for current input, continuation anchors, and compatibility windows. Turn finalization reads messages to populate `TurnRecord.input_message_ids`. Runtime recovery and task output helpers scan messages for replay and correlation. |
| `queue_entries.jsonl` | `Runtime::enqueue`, dequeue/processing paths, interjection handling, and scheduler tests append queue lifecycle snapshots for a `message_id`. | Scheduler and work-queue projection use latest queue state to decide runnable/queued items. Delivery helpers use it to bind queued/admitted messages to later output. |
| `turns.jsonl` | Turn finalization appends a lightweight `TurnRecord` after a terminal turn result or failure path has enough linkage evidence. | Prompt projection should increasingly use it as the recent-turn causal spine. Tests and diagnostics read it to verify turn linkage. |
| `transcript.jsonl` | Runtime turn execution, message dispatch, failure handling, subagent handling, and host bootstrap append model-facing entries and assign `transcript_seq`. | Prompt context currently reads recent transcript as a compatibility/context source. Runtime lifecycle and tests use it for provider round recovery and model-facing trace assertions. |
| `tools.jsonl` | Tool execution paths append `ToolExecutionRecord`; command-like tools also mark the memory index dirty. | Prompt context, working memory projection, task output, and tests read recent tool evidence so agents can recover side effects without rerunning tools. |
| `briefs.jsonl` | Runtime delivery, memory refresh, task reducer, tests, and completion paths append generated `BriefRecord` summaries. | Prompt context, delivery APIs, memory indexing, scheduler signals, and turn finalization read briefs as result/failure/status evidence. Current code still includes `Ack` briefs, but this RFC treats ordinary acks as a design gap. |
| `delivery_summaries.jsonl` | Completion and delivery paths append `DeliverySummaryRecord`. | Work item query tools, run-once helpers, delivery helpers, and turn finalization use it to bind user-facing closure back to turns and work items. |
| `events.jsonl` | `Runtime::append_audit_event`, HTTP/operator surfaces, scheduler, wait mirroring, turn finalization, command-task handling, and lifecycle paths append audit events and assign `event_seq`. | HTTP event streams, diagnostics, lifecycle counters, scheduler signals, recovery tests, and TUI/debug views use it for cursorable audit evidence. It is not the canonical domain store. |
| `tasks.jsonl` | Task reducer, command task, child-agent supervision, task tools, tests, and run-once fixtures append task lifecycle snapshots. | Recovery snapshots, task list/status/output APIs, scheduler blocking checks, memory indexing, and prompt/work item projections reconstruct latest task state from this history. |
| `work_items.jsonl` | Work item tools, lifecycle APIs, wait helpers, task helpers, memory refresh, and tests append work item revisions/state snapshots. | Work queue projection, scheduler readiness, prompt current/queued/blocked sections, memory indexing, work item query tools, and recovery reconstruct latest state by work item id. |
| `work_item_delegations.jsonl` | Task completion/delegation paths and tests append parent/child delegation state. | Recovery and latest-delegation helpers reconstruct child-agent ownership and delegation lifecycle. |
| `timers.jsonl` | Wait/timer runtime paths append timer creation, update, and fire-state records. | Scheduler and recovery use latest timer state and fire counters to decide timer wakes. |
| `waiting_intents.jsonl` | Legacy wait APIs and compatibility paths append wait intent records; `append_waiting_intent` also mirrors to `wait_conditions.jsonl`. | Prompt context and memory refresh still read latest active waiting intents for compatibility. This overlaps with wait conditions and is a refactor target. |
| `wait_conditions.jsonl` | `WaitFor`/waiting runtime paths append scheduler-visible wait conditions; legacy waiting intents mirror into this file. | Scheduler readiness, work queue projection, turn finalization, and work item tools use latest active/resolved wait conditions to decide blocked/runnable state. |
| `external_triggers.jsonl` | Callback/external trigger runtime paths append trigger creation, update, and revocation records. | Prompt context renders default ingress metadata; callback resolution and work item wait flows use latest trigger state to wake agents. Capability-bearing fields must not be projected carelessly. |
| `operator_notifications.jsonl` | Operator notification APIs append notification lifecycle records. | Operator APIs read recent notifications for display and delivery decisions. |
| `operator_transport_bindings.jsonl` | Operator transport binding APIs append binding records. | Operator delivery and storage helpers read latest bindings to route user-facing output. |
| `operator_delivery_records.jsonl` | Operator delivery APIs append submitted/completed delivery attempt records. | Operator APIs and delivery diagnostics read recent/latest delivery attempts per surface. |
| `working_memory_deltas.jsonl` | Working memory APIs append memory state deltas. | Working memory projection rebuilds current memory from deltas; prompt context uses the projected memory, not raw deltas. |
| `context_episodes.jsonl` | Episode compaction and memory episode paths append compacted context episodes. | Prompt context and memory index rebuild read episodes as long-lived context evidence and recovery anchors. |
| `workspaces.jsonl` | Runtime bootstrap, host registry, lifecycle workspace APIs, and tests append workspace registry entries. | Workspace registry, memory indexing, and lifecycle APIs reconstruct available workspaces and active project context. |
| `workspace_occupancies.jsonl` | Host registry appends occupancy enter/leave records. | Host/runtime coordination uses occupancy history to reason about active workspace ownership. |
| `agent_identities.jsonl` | Host and host registry append identity/profile records for public and child agents. | Host/runtime identity lookup and recovery reconstruct known agents and their profile/ownership history. |

Implementation-level observations:

- `messages.jsonl`, `transcript.jsonl`, and `events.jsonl` currently have
  monotonic sequence assignment in `AppStorage`; most other ledgers rely on
  record ids, revisions, timestamps, or append history.
- Appending briefs, tasks, work items, selected command tool executions,
  context episodes, and workspace entries marks the memory index dirty because
  memory search/projection may need to rebuild from those ledgers.
- `waiting_intents.jsonl` is already a compatibility layer in code:
  `append_waiting_intent` writes the legacy intent and mirrors a derived
  `WaitConditionRecord`.
- `poll_activity_marker` currently watches briefs, tasks, tools, events, and
  transcript. It is an activity-detection surface, not a complete ledger
  inventory.

## Ledger classes

### 1. Admission ledgers

Admission ledgers record how work enters the runtime.

```text
messages.jsonl
queue_entries.jsonl
operator_transport_bindings.jsonl
external_triggers.jsonl
```

`messages.jsonl` is the canonical source for admitted message content. It must
preserve origin, authority class, priority, trigger kind, delivery surface,
admission context, and source references.

`queue_entries.jsonl` records whether an admitted message is queued, dequeued,
processed, interjected, aborted, or dropped. It should not duplicate message
body content except by reference to `message_id`.

`external_triggers.jsonl` records callback or external wake capabilities and
their lifecycle. Capability-bearing data must remain protected and should not be
copied into prompt projection.

### 2. Execution ledgers

Execution ledgers record what one activation did.

```text
turns.jsonl
transcript.jsonl
tools.jsonl
events.jsonl
```

`turns.jsonl` should be the causal spine. A turn should identify the runtime
activation and reference the message, tool, brief, delivery, completion, and
wait records produced or consumed by that activation.

`transcript.jsonl` is the model-facing trace. It records what was presented to
or received from the model/provider. It is not the canonical input ledger
because it may include runtime system content, projected context, internal
assistant text, provider retries, and compacted conversation state.

Transcript entries may include both input-side and output-side material:
operator/user entries, assistant responses, tool call/result representations,
system/developer/context projection sections, and provider round history. They
should be read as model-facing conversation evidence, not as a replacement for
`messages.jsonl` or the domain ledgers.

`tools.jsonl` is side-effect evidence. It should preserve enough invocation,
status, output preview, artifact reference, and provenance data to let future
agents recover what was done without re-running side effects.

`events.jsonl` is an audit/event stream. It is useful for cursors, SSE, debug,
and replay diagnostics, but should not replace domain ledgers as the canonical
source of message, task, work item, wait, or tool state.

### 3. Outcome and delivery ledgers

Outcome ledgers record what the agent or runtime reported.

```text
briefs.jsonl
delivery_summaries.jsonl
operator_notifications.jsonl
operator_delivery_records.jsonl
```

`briefs.jsonl` should contain outcome-bearing or state-summary records, such as
results, failures, waits, blocked states, verification summaries, and completion
reports.

It should not contain raw operator input as a normal brief. Operator input is an
authority-bearing message and belongs in `messages.jsonl`, optionally referenced
from a `TurnRecord`.

Ordinary runtime acknowledgements such as `Queued work: ...` are not task
outcomes. They are admission or lifecycle evidence. Prompt projection should
usually omit them once the same turn has the original input and a terminal
result/failure/wait. Long term, this RFC recommends moving ordinary admission
acknowledgements out of the brief concept and into queue or turn lifecycle
records.

`delivery_summaries.jsonl` records user-facing closure and completion delivery.
`operator_notifications.jsonl` and `operator_delivery_records.jsonl` record
operator notification and transport delivery lifecycle. These records should
reference the brief, message, work item, or turn they deliver rather than
becoming independent content sources.

### 4. Lifecycle state ledgers

Lifecycle ledgers store append histories for mutable runtime objects.

```text
tasks.jsonl
work_items.jsonl
work_item_delegations.jsonl
timers.jsonl
waiting_intents.jsonl
wait_conditions.jsonl
```

These files are append-only at the storage layer, but most of them are
semantically latest-state histories. Readers should reconstruct current state by
object id and, where available, revision/status/update time.

The object id is the stable identity. Append order is storage history.
`revision` should represent object version where the object is mutable.
Parent-local fields such as timer `fire_count` remain local counters, not
ledger-wide ordering.

`wait_conditions.jsonl` should be the scheduler-visible wait surface. It should
anchor blocked/runnable decisions and should be referenced by turn records when
a turn intentionally yields.

`waiting_intents.jsonl` is historical intent evidence and may be narrowed or
merged with `wait_conditions.jsonl` if the wait model is simplified.

### 5. Context and identity ledgers

Context ledgers store long-lived agent context and runtime ownership state.

```text
working_memory_deltas.jsonl
context_episodes.jsonl
workspaces.jsonl
workspace_occupancies.jsonl
agent_identities.jsonl
```

`working_memory_deltas.jsonl` records memory state changes. It should remain
separate from prompt summaries because memory has its own authority and curation
rules.

`context_episodes.jsonl` records compaction episodes, boundary summaries, and
recovery anchors. Episodes are useful context evidence, but they should not
override original message, tool, or turn records when those records are still
available.

Workspace and identity ledgers record runtime configuration and occupancy. They
are context for execution, not substitutes for task or message ledgers.

## Source-of-truth rules

### Operator and external input

Canonical source:

```text
messages.jsonl
```

Supporting records:

```text
queue_entries.jsonl
turns.jsonl
transcript.jsonl
events.jsonl
```

Rules:

- `messages.jsonl` contains admitted runtime messages: operator inputs,
  external wakes, queued continuations, task-result rejoins, timer/system wakes,
  and self-enqueued follow-ups. It does not contain every runtime record.
- Preserve operator input in the message ledger with original text and
  authority metadata.
- Do not store operator input as `BriefKind::Input`.
- Do not rely on transcript entries as the only copy of admitted input.
- A turn should reference admitted inputs through `input_message_ids`.
- Prompt projection should prefer the message record for operator input and
  should mark any truncation explicitly.

### Turn execution

Canonical source:

```text
turns.jsonl
```

Supporting records:

```text
messages.jsonl
tools.jsonl
briefs.jsonl
delivery_summaries.jsonl
wait_conditions.jsonl
work_items.jsonl
tasks.jsonl
transcript.jsonl
```

Rules:

- A turn is the causal container for one runtime activation.
- A turn should reference consumed input, produced tool records, produced
  briefs, delivery summaries, completed work items, and wait conditions.
- A turn links inputs by `MessageEnvelope.id`, but links outputs and side
  effects by their own domain record ids: `ToolExecutionRecord.id`,
  `BriefRecord.id`, `DeliverySummaryRecord.id`, work item id, task id, and wait
  condition id.
- A turn should not duplicate full message bodies, tool output, or brief text
  except for compact summaries needed by recovery.
- If a historical turn record is missing, prompt projection may reconstruct a
  best-effort turn from message, brief, tool, transcript, and queue records, but
  that reconstruction is a projection, not a new canonical fact.

### Tool evidence

Canonical source:

```text
tools.jsonl
```

Supporting records:

```text
turns.jsonl
transcript.jsonl
events.jsonl
```

Rules:

- Tool execution records should be sufficient to identify the tool call, trusted
  origin, status, bounded result preview, and full artifact references.
- Prompt projection can compact successful tool calls to evidence rows.
- Failed, cancelled, truncated, artifact-producing, or still-running tool calls
  should remain visible enough for recovery.

### Briefs and results

Canonical source:

```text
briefs.jsonl
delivery_summaries.jsonl
```

Supporting records:

```text
turns.jsonl
operator_delivery_records.jsonl
transcript.jsonl
events.jsonl
```

Rules:

- Briefs are generated summaries or runtime status records, not raw authority
  inputs.
- Result and failure briefs are terminal outcome evidence.
- A completion report may be both a brief-like user-facing result and a work
  item lifecycle closure; records should preserve both references.
- Prompt projection should prefer terminal result/failure/wait/completion
  records over admission acknowledgements.

### Work item and task lifecycle

Canonical source:

```text
work_items.jsonl
tasks.jsonl
work_item_delegations.jsonl
```

Supporting records:

```text
turns.jsonl
wait_conditions.jsonl
briefs.jsonl
delivery_summaries.jsonl
events.jsonl
```

Rules:

- Lifecycle ledgers are append histories, but readers usually want latest state
  per object id.
- Work item state should remain durable across turns and should not be inferred
  solely from briefs.
- Child task and delegation records should be linked to work item state when
  they affect readiness, completion, or cleanup.

### Wait and wake state

Canonical source:

```text
wait_conditions.jsonl
external_triggers.jsonl
timers.jsonl
```

Supporting records:

```text
waiting_intents.jsonl
queue_entries.jsonl
messages.jsonl
turns.jsonl
events.jsonl
```

Rules:

- Wait conditions are scheduler-visible blockers.
- External triggers and timers are wake capabilities or wake sources.
- Wake messages should be admitted as messages and linked back to their wait or
  trigger records through source references.
- Prompt projection should distinguish operator instructions from external,
  timer, task-result, and system wakes.

## Relationship model

The intended high-level graph is:

```text
MessageEnvelope(message_id)
  └─ QueueEntryRecord(message_id)
      └─ TurnRecord(turn_id, input_message_ids[])
          ├─ TranscriptEntry(turn_id?, message_id?)
          ├─ ToolExecutionRecord(tool_execution_id, turn_id?)
          ├─ BriefRecord(brief_id, turn_id?, related_message_id?, related_task_id?)
          ├─ DeliverySummaryRecord(delivery_summary_id, turn_id?, brief_id?)
          ├─ WorkItemRecord(work_item_id, revision?)
          ├─ TaskRecord(task_id)
          └─ WaitConditionRecord(wait_condition_id)

ExternalTriggerRecord / TimerRecord / TaskRecord
  └─ MessageEnvelope(source_refs, trigger_kind)
      └─ TurnRecord(...)

AuditEvent(event_seq)
  └─ references domain ids for debug, SSE, and diagnostics
```

The graph should preserve two boundaries:

1. A reference does not transfer authority. A model-generated brief that
   references an operator message does not become operator instruction.
2. A projection does not become source of truth. `recent_turns`,
   `context_episodes`, and event-stream summaries can explain prior state but
   should not override canonical domain records.

## Ordering and revision model

This RFC adopts the existing direction from
[Runtime Ledger Sequences and Object Revisions](./runtime-ledger-sequences-and-revisions.md):

- Object IDs are opaque identity, not ordering.
- `message_seq` orders admitted messages.
- `transcript_seq` orders transcript entries.
- `event_seq` orders audit events and stream cursors.
- `turn_index` orders agent activations for one agent.
- `revision` represents mutable object version where the object is updated over
  time.
- Timestamps are useful metadata, not the only ordering contract for replay.
- Parent-local counters such as transcript round, batch item index, and timer
  fire count remain local.

Not every ledger needs a sequence immediately. The decision should depend on
whether clients need durable paging, replay, or cursor semantics over that
ledger.

## Prompt projection boundary

Prompt context should move toward a turn-first projection:

```text
current input
current WorkItem state
recent_turns
relevant episodes / memory
active waits and tasks
```

`recent_turns` should be built from `TurnRecord` when available. Each rendered
turn should include compact, provenance-aware fields such as:

- `turn_id` or `turn_index`
- trigger kind and continuation relation
- operator input or wake evidence
- current work item id
- produced result/failure/wait/completion briefs
- tool execution evidence rows
- completion or wait state
- recovery anchors such as message ids, tool execution ids, command refs, brief
  ids, and work item ids

If `TurnRecord` is unavailable, projection may reconstruct a turn from related
message, brief, tool, transcript, and queue records. That fallback should be
explicitly treated as a compatibility projection.

The older independent windows:

```text
recent_messages
recent_briefs
recent_tool_executions
latest_result
```

should become compatibility fallbacks or debugging surfaces once `recent_turns`
is reliable enough. They should not remain equal peers that can contradict the
turn spine.

## Current design gaps

The current implementation has several known gaps:

- `briefs.jsonl` still includes ordinary `Ack` records, which are closer to
  admission lifecycle than outcome summaries.
- `BriefKind` is narrow (`Ack`, `Result`, `Failure`) while runtime behavior has
  more states such as wait, blocked, verification, and completion.
- `TurnRecord` is lightweight and may not yet cover every record relation needed
  by prompt projection and recovery.
- Historical records may have `turn_index` without `turn_id`, or related ids
  without a complete turn record.
- `waiting_intents.jsonl` and `wait_conditions.jsonl` overlap conceptually.
- Some lifecycle ledgers use append histories without a consistently documented
  `revision` policy.
- Lifecycle JSONL files currently mix log and state semantics: they append
  latest-state snapshots and then reconstruct current state by latest-record
  rules.
- `events.jsonl` can look like a universal ledger even though it should remain
  an audit/event-stream mirror.
- Prompt context still contains parallel recent windows that can duplicate or
  flatten causal context.

## Database-backed state storage direction

If compatibility with existing JSONL files is temporarily ignored, the storage
model should be redesigned around explicit database state tables rather than a
universal event-sourced event log.

The target split is:

```text
state_tables
  canonical current Holon-owned lifecycle state
  records "what the state is now"

evidence_records
  immutable runtime evidence
  records "what the runtime admitted, saw, rendered, invoked, or produced"

audit_events
  observer-facing stream
  records "what observers may want to follow"
```

This gives each storage layer one responsibility:

```text
state table = recoverable runtime state owned by Holon
evidence    = immutable input/output/tool/model/provenance trace
audit event = cursorable observer/debug signal
```

The database target is therefore not:

```text
domain_events -> reducers -> projections
```

It is instead:

```text
domain service command
  -> validate current row, authority, idempotency, and revision
  -> update canonical current-state table in the same transaction
  -> record evidence references and causation/correlation metadata
  -> append observer-facing audit event
```

The current-state table is the source of truth for Holon-owned state. Evidence
and audit explain why that state exists and what happened around it, but they
are not replayed to derive canonical state.

### `events.jsonl` is audit, not state source

Current `events.jsonl` stores `AuditEvent` records. Its schema is intentionally
generic:

```text
id
event_seq
created_at
kind
data
```

It is useful for cursors, SSE, diagnostics, lifecycle counters, scheduler
signals, and TUI/debug views. It is not a safe canonical state source because:

- `kind + data` does not define a strong domain-state contract.
- Many audit events are debug or scheduler notices, not lifecycle state.
- Payloads are not guaranteed to contain complete state, preconditions,
  causation, correlation, and idempotency metadata.
- Some state transitions may be missing or duplicated in the audit stream.
- Some audit events are derived observer signals and should not affect recovery.

The target model should therefore rename or document `events.jsonl` as
`audit_events`. It may reference state rows and evidence rows, but it must not
be promoted into the state-recovery source of truth.

### Canonical current-state tables

Holon-owned lifecycle state should be represented by normal database tables with
stable primary keys, explicit status fields, revision columns, and provenance
references. These tables replace hot-path latest-snapshot scans.

Good first candidates are:

```text
work_items
tasks
queue_entries / message_queue
wait_conditions
timers
external_triggers
agents
workspaces
operator_notifications
operator_transport_bindings
operator_delivery_state
```

`external_triggers` should remain a standalone current-state table in the first
DB migration. That matches the existing `external_triggers.jsonl` domain and
keeps capability lifecycle import mechanical. A later refactor may fold the
single default agent ingress into `agents` or split secrets behind a dedicated
capability service, but that is not part of the initial cutover.

A state table row should normally include:

```text
id / domain-specific primary key
agent_id / owner scope
state or status
revision
created_at
updated_at
created_turn_id / last_turn_id
created_message_id / last_message_id
causation_id
correlation_id
evidence_refs_json
metadata_json / payload_json
```

`revision` replaces the current JSONL latest-scan revision pattern as an
optimistic concurrency and monotonic update guard. The database row is not a
projection derived from a hidden event stream; it is the canonical current state.

Example table shapes:

```sql
CREATE TABLE work_items (
  work_item_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  state TEXT NOT NULL,
  objective TEXT NOT NULL,
  plan_status TEXT,
  readiness TEXT,
  current_focus INTEGER NOT NULL DEFAULT 0,
  revision INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  created_turn_id TEXT,
  last_turn_id TEXT,
  created_message_id TEXT,
  last_message_id TEXT,
  causation_id TEXT,
  correlation_id TEXT,
  evidence_refs_json TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE tasks (
  task_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  status TEXT NOT NULL,
  summary TEXT,
  accepts_input INTEGER,
  output_path TEXT,
  revision INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  created_turn_id TEXT,
  last_turn_id TEXT,
  created_message_id TEXT,
  last_message_id TEXT,
  causation_id TEXT,
  correlation_id TEXT,
  evidence_refs_json TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE wait_conditions (
  wait_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  scope TEXT NOT NULL,
  work_item_id TEXT,
  task_id TEXT,
  resource TEXT,
  status TEXT NOT NULL,
  wake_kind TEXT NOT NULL,
  recheck_after TEXT,
  triggered_at TEXT,
  revision INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  created_turn_id TEXT,
  last_turn_id TEXT,
  causation_id TEXT,
  correlation_id TEXT,
  evidence_refs_json TEXT,
  payload_json TEXT NOT NULL
);

CREATE TABLE timers (
  timer_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  status TEXT NOT NULL,
  wake_at TEXT NOT NULL,
  work_item_id TEXT,
  revision INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  created_turn_id TEXT,
  last_turn_id TEXT,
  causation_id TEXT,
  correlation_id TEXT,
  evidence_refs_json TEXT,
  payload_json TEXT NOT NULL
);
```

Indexes should be optimized for scheduler and API reads, for example:

```sql
CREATE INDEX idx_work_items_agent_state
  ON work_items (agent_id, state, updated_at);

CREATE INDEX idx_tasks_agent_status
  ON tasks (agent_id, status, updated_at);

CREATE INDEX idx_wait_conditions_status_recheck
  ON wait_conditions (status, recheck_after);

CREATE INDEX idx_timers_status_wake_at
  ON timers (status, wake_at);
```

### State write contract

Because state tables are canonical mutable state, writes must be constrained by
repository or domain-service APIs rather than scattered SQL updates.

Every state-changing command should run in one transaction that:

1. Reads the current row and validates status, revision, authority, and
   idempotency.
2. Applies the state transition to the canonical current-state table.
3. Increments `revision` and updates `updated_at`.
4. Records causation, correlation, turn, message, and evidence references.
5. Appends an audit event for observers and diagnostics.
6. Commits atomically.

For example:

```text
CompleteWorkItem command
  -> WorkItemService::complete(...)
    BEGIN
      read work_items where id = ? and revision = ?
      validate state = open
      validate authority and idempotency key
      update work_items
        set state = 'completed',
            revision = revision + 1,
            updated_at = now,
            last_turn_id = ?,
            last_message_id = ?,
            evidence_refs_json = ?
      insert audit_events(kind = 'work_item_completed', ...)
    COMMIT
```

This preserves the main benefit of the current revision mechanism without
pretending the append history is the canonical state machine.

### State history and diagnostics

Some lifecycle streams may still need history for diagnostics, UI timeline, or
forensic audit. That does not require domain events. Options include:

- append observer-facing `audit_events` with state row ids and revisions;
- keep immutable evidence records for the command, message, tool, or turn that
  caused the write;
- optionally maintain narrow `*_history` tables that store before/after
  snapshots or changed fields for debugging.

Those history records are not the source of truth. They support explanation,
not recovery. Recovery should load the current state tables directly.

### Evidence records in the target model

Not every append-only fact is state. These should remain evidence or trace
records, with indexed lookup and turn hydration support:

```text
messages
transcript
tool executions
turn details
model requests and responses
command results
web fetches
briefs
delivery summaries
provider traces
artifact metadata
context episodes
working memory evidence
```

For example:

- `messages` preserves authority-bearing admitted input and wake envelopes.
- `transcript` preserves model-facing context and provider conversation trace.
- `tools` preserves side-effect invocation/result evidence.
- `briefs` and `delivery_summaries` preserve generated outcome and delivery
  evidence.
- `context_episodes` preserves compaction/recovery anchors.

These records can be stored in specialized tables or a shared
`evidence_records` table with indexes by record id, turn id, message id, task
id, work item id, and agent id. They should not be replayed to recover task,
work item, wait, queue, or timer state.

### Example write flow

Creating a work item in the target model should not append a full
`WorkItemRecord` snapshot as the canonical fact. It should write one
transaction like:

```text
state_tables:
  insert work_items(work_1, state=open, objective=..., revision=1, ...)

evidence:
  reference admitted message / turn / command evidence

audit_events:
  append observer/debug event "work_item_created"
```

Changing the objective:

```text
state_tables:
  update work_items
    set objective = ..., revision = revision + 1, last_turn_id = ...
    where work_item_id = ? and revision = ?

evidence:
  reference the message or command evidence that caused the change

audit_events:
  append observer/debug event "work_item_updated"
```

Completing the work item:

```text
state_tables:
  update work_items
    set state = 'completed', revision = revision + 1, last_turn_id = ...
    where work_item_id = ? and revision = ?

evidence:
  reference delivery summary / turn / operator-facing completion evidence

audit_events:
  append observer/debug event "work_item_completed"
```

Recovery then becomes:

```text
open database
load work_items, tasks, waits, timers, queue entries, agents, and workspaces
hydrate recent evidence by id when prompt projection, diagnostics, or APIs need it
resume scheduler from current state tables
```

There is no mandatory replay step. Historical JSONL files can be imported into
current-state tables during migration, but the steady-state source of truth is
the database row.

### Design constraints

The database-backed design should preserve these constraints:

- Holon-owned lifecycle state lives in canonical state tables.
- State table writes go through domain services or repositories.
- State transitions validate current status, revision, authority, and
  idempotency.
- Evidence records store full input/output/provenance data.
- Audit events may duplicate display-friendly summaries, but cannot become the
  state recovery source.
- Capability secrets and other protected data must be redacted or referenced
  safely before entering evidence or audit payloads.
- JSONL lifecycle histories are compatibility/import evidence, not the target
  source of truth.

## Refactor direction

The refactor should be staged so that old ledgers remain readable.

### Phase 1: Document and enforce projection boundaries

- Treat `messages.jsonl` as the canonical source for raw input.
- Treat `turns.jsonl` as the primary prompt projection spine.
- Stop treating ordinary `Ack` briefs as prompt-relevant produced briefs.
- Keep independent recent windows only as compatibility or debug fallback.
- Make fallback reconstruction from message/brief/tool ledgers explicit.

### Phase 2: Strengthen turn linkage

- Ensure new message, tool, brief, delivery, wait, task, and work item lifecycle
  changes can be linked to a turn when they are caused by a turn.
- Prefer `turn_id` for durable joins and keep `turn_index` as agent-local
  ordering.
- Preserve recovery anchors in prompt rendering whenever content is trimmed.

### Phase 3: Normalize brief semantics

- Keep briefs for outcome-bearing and state-summary records.
- Do not introduce `BriefKind::Input`.
- Move ordinary admission acknowledgements toward queue or turn lifecycle
  records.
- Consider expanding brief kinds only for semantically meaningful outcomes such
  as wait, blocked, verification, completion, and failure.

### Phase 4: Normalize lifecycle histories

- Document latest-state reconstruction rules per lifecycle ledger.
- Add `revision` where mutable object histories need stable object-version
  semantics.
- Keep append-only storage but make latest-state readers explicit.
- Revisit overlap between `waiting_intents.jsonl` and `wait_conditions.jsonl`.

### Phase 5: Revisit audit/event scope

- Keep `events.jsonl` as cursorable audit/event-stream evidence.
- Avoid using audit events as the only durable copy of domain state.
- Add domain references to events where they help debugging and SSE consumers.
- Keep event schema broad enough for diagnostics but narrow enough that domain
  ledgers remain authoritative.

### Phase 6: Introduce database-backed state and indexed evidence

- Add canonical current-state tables for hot lifecycle reads such as work items,
  tasks, queue, waits, timers, external triggers, agents, and workspaces.
- Route state writes through domain services or repositories that enforce
  revision, authority, idempotency, provenance, and audit emission.
- Keep messages, transcript, tools, briefs, delivery summaries, and context
  records as evidence ledgers with indexed lookup.
- Treat current JSONL lifecycle snapshots as compatibility import sources and
  migration evidence.
- Keep `events.jsonl` or its successor as `audit_events`, a mirror for
  observers and diagnostics rather than state replay.
- Do not introduce `domain_events` as a mandatory storage layer; direct
  current-state tables are sufficient for the first database model.

## Compatibility policy

Readers should remain tolerant of historical ledgers:

- Records may be missing `turn_id`.
- Records may have only `turn_index`.
- `Ack` briefs may exist and should usually be ignored in prompt projection when
  better evidence exists.
- Some ledgers may rely on JSONL order before sequence fields exist.
- Some mutable histories may lack explicit revisions.
- Reconstruction code may need to group by message id, related task id, work
  item id, turn index, timestamps, or transcript round when no single join key
  exists.

Compatibility fallback should not define the target model. It should be a
temporary bridge toward turn-linked ledgers.

## Open questions

1. Should `TurnRecord` grow explicit fields for task ids, work item revision ids,
   queue entry ids, and transcript entry ids?
2. Should `BriefKind` expand beyond `Ack`, `Result`, and `Failure`, or should
   wait/completion/verification be represented elsewhere?
3. Should ordinary admission acknowledgement records be removed entirely, or
   migrated into queue/turn lifecycle records?
4. Should `waiting_intents.jsonl` be merged into `wait_conditions.jsonl`, kept
   as historical intent evidence, or removed after migration?
5. Which lifecycle ledgers need `revision` immediately?
6. Which ledgers need sequence fields beyond `events`, `messages`,
   `transcript`, and `turn_index`?
7. Should delivery summaries and operator delivery records be consolidated or
   kept separate because they represent different layers?
8. Should prompt projection expose explicit recovery commands or only expose
   durable ids and artifact refs?
9. Which current-state tables should be migrated first: work items, tasks,
   waits, queue, timers, external triggers, agents, or workspaces?
10. Which JSONL lifecycle histories should remain as compatibility import
    evidence after their corresponding database state tables exist?

## Discussion conclusion: database storage model

The discussion following this draft narrows the database-backed direction:
Holon should not make strict event sourcing the default storage model for the
whole agent system.

The preferred near-term model is:

```text
immutable evidence records
  what the runtime admitted, observed, invoked, rendered, or received

canonical current-state tables
  what Holon-owned lifecycle state is now

audit events
  what observers, TUI, web clients, SSE streams, and diagnostics should follow
```

In this model, state tables are the source of truth for Holon-owned lifecycle
state. They should be updated through domain services that enforce validation,
revision checks, idempotency, provenance, evidence references, and audit
emission. They should not be updated through scattered ad hoc storage writes.

Immutable evidence records should preserve observations and side-effect
evidence such as admitted messages, tool executions, web fetches, command
results, transcripts, prompt artifacts, model responses, briefs, delivery
summaries, external observations, and artifact metadata. They let Holon explain
what the model saw and what tools reported without replaying external side
effects.

Audit events should remain observer-facing records. They may reference state
objects and evidence records, and they should be useful for TUI/web timelines,
SSE cursors, debugging, and lifecycle counters. They are not the recovery source
for canonical state.

`domain_events` are not part of the target storage contract. They add a second
source of truth unless every state transition is fully reducer-backed and every
current table is treated as a rebuildable projection, which is unnecessary for
Holon's near-term runtime needs. Work items, tasks, waits, queue entries, and
timers should be maintained directly in database state tables. Their histories
should be captured through audit events and evidence references, not through a
mandatory domain event log.

This changes the target split from event sourcing to a simpler storage
contract:

```text
state tables = canonical current Holon-owned state
evidence     = immutable observations, traces, and side-effect records
audit_events = cursorable observer/debug stream
```

The recovery target is therefore current-state recovery plus causal evidence
hydration, not deterministic replay of the whole agent system. Holon should be
able to restore its own lifecycle state and explain why that state exists, but
it should not try to replay model calls, web fetches, command executions, or
external systems as if they were database mutations.

## Expected outcome

After this RFC is accepted, Holon should have a clear contract for adding or
refactoring ledger files:

- every new ledger must declare its source-of-truth role
- every projection or read model must declare the canonical records it reads
  from
- every lifecycle history or state table must define the source of current state
- every prompt surface must preserve authority and provenance
- every cross-ledger relation should use explicit ids rather than implicit
  timestamp proximity

This should make the next ledger refactor a sequence of bounded migrations
rather than another accumulation of parallel context surfaces.
