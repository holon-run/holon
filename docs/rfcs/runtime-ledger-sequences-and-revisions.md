---
title: RFC: Runtime Ledger Sequences and Object Revisions
date: 2026-05-29
status: draft
---

# RFC: Runtime Ledger Sequences and Object Revisions

## Summary

Holon should use explicit ordering and version fields where runtime records need
durable cursor, replay, paging, or lifecycle-version semantics.

This RFC separates three concepts that are easy to conflate:

- append-only ledger sequence fields named `*_seq`
- mutable object version fields named `revision`
- parent-local ordering fields named `index`, `part_index`, `round`, or another
  parent-scoped name

Runtime object IDs remain opaque identifiers and should not carry ordering
semantics. Short random ID generation is covered by
[Runtime ID Generation](./runtime-id-generation.md). This RFC only defines when
Holon records should add sequence, revision, or local index fields alongside
those IDs.

## Problem

Holon already has several ordering and version fields:

- `AuditEvent.event_seq` for event-stream cursor and SSE ordering
- `AgentState.turn_index` for agent turn progression
- `WorkItemRecord.revision` for mutable WorkItem lifecycle updates
- `WorkingMemoryState.working_memory_revision` and memory deltas for memory
  state transitions
- local counters such as transcript `round`, timer `fire_count`, wait
  `trigger_count`, and command batch item indexes

But the rule is not yet explicit. Many records still rely on `id`, `created_at`,
and JSONL append order even when future clients may need stable cursors or
replay boundaries.

The missing policy creates recurring design questions:

- should a record get a shorter ID, a sequence number, or both?
- should an append-only log use JSONL order or an explicit cursor?
- should mutable lifecycle records use timestamps or an explicit revision?
- when is a local child index enough?
- which fields should be added now, and which should wait for a concrete
  product need?

Without a policy, Holon risks either over-indexing every object or continuing to
add one-off counters that do not compose across runtime surfaces.

## Goals

- define when append-only ledgers should have durable `*_seq` fields
- define when mutable records should have `revision` fields
- preserve parent-local ordering fields for child records that do not need
  ledger-wide identity
- keep sequence/revision design separate from runtime ID generation
- prioritize the first useful additions without forcing a broad migration
- keep old records readable during incremental adoption

## Non-goals

- do not replace runtime object IDs with counters
- do not change the short random ID policy from the runtime ID RFC
- do not require every JSONL file to gain a sequence field immediately
- do not add `seq` to records that only need parent-local ordering
- do not define distributed or cross-agent global ordering
- do not migrate historical ledgers solely to backfill sequence fields unless a
  reader or API needs a cursor over historical data
- do not change `seq` behavior in the runtime ID RFC implementation

## Terminology

### Runtime object ID

An opaque handle for a runtime object, such as `task_...`, `msg_...`, or
`tool_...`.

IDs answer:

```text
Which object is this?
```

They should not answer:

```text
Where does this object appear in an append-only stream?
Which version of this mutable object is current?
What is this child's position inside its parent?
```

### Ledger sequence

A ledger sequence is a durable append position inside one append-only ledger.
It is used for replay, paging, stable ordering, and cursor APIs.

Naming rule:

```text
<ledger_name>_seq
```

Examples:

```text
event_seq
message_seq
transcript_seq
tool_seq
brief_seq
```

Sequence scope should be explicit. Unless another scope is named, Holon ledger
sequences are per-agent ledger sequences, not global counters.

### Object revision

An object revision is a durable version of one mutable logical object.

Naming rule:

```text
revision
```

Examples:

```text
WorkItemRecord.revision
TaskRecord.revision
WaitConditionRecord.revision
WorkspaceEntry.revision
```

Revisions answer:

```text
Which update of this object is this?
```

They do not provide ordering across unrelated objects.

### Parent-local index

A parent-local index orders a child within its parent object or local operation.
It is not a ledger cursor and should not be used as a public replay boundary.

Examples:

```text
round
part_index
chunk_index
batch_item.index
artifact.index
```

Parent-local indexes answer:

```text
Where is this child inside this parent?
```

## Policy

### Add `*_seq` to append-only ledgers that need durable cursors

Use a `*_seq` field when all of these are true:

1. the storage surface is append-only or append-mostly;
2. consumers may need stable replay, pagination, deduplication, or `after_seq`
   semantics;
3. timestamp ordering is not sufficient because clocks can collide, backfill can
   occur, or append order should be explicit;
4. JSONL physical order should not be the only externally meaningful ordering
   contract.

The sequence should be assigned by the append path that owns the ledger. Callers
should not provide arbitrary sequence numbers for new records.

The sequence should be monotonically increasing within the ledger scope. It does
not need to be gap-free if crash recovery, compaction, or partial writes make
gap-free semantics expensive, but the append path should not intentionally reuse
a sequence number.

### Add `revision` to mutable lifecycle objects

Use `revision` when a logical object can be updated over time and observers need
to distinguish one state from a later state.

Good candidates:

- lifecycle records that transition through statuses
- records that can be observed by clients while they are changing
- records where `updated_at` is useful for display but too weak for versioning
- records where stale writes or repeated projections would benefit from an
  optimistic version field

The first persisted version should use `revision = 1`. Each semantic update to
the object should increment the revision by one.

### Keep parent-local fields local

Use local indexes when ordering is only meaningful inside one parent:

- transcript round inside one model turn
- content part index inside one message
- command batch item index inside one tool execution
- artifact index inside one tool result
- chunk index inside one streamed or paged payload

These fields should not be promoted to ledger sequences unless external replay
or paging needs to cross parent boundaries.

### Keep IDs and order/version fields independent

A record may have both an ID and a sequence or revision:

```rust
MessageEnvelope {
    id: MessageId,
    message_seq: u64,
    created_at: DateTime<Utc>,
    // ...
}
```

The ID remains the object reference. The sequence is only the append position.
Clients should not infer object type, authorization, lifecycle status, or parent
relationship from either field alone.

## Current State

Holon already has several fields that fit this model:

| Field | Category | Scope |
| --- | --- | --- |
| `AuditEvent.event_seq` | ledger sequence | per-agent event ledger |
| `AgentState.turn_index` | execution sequence | per-agent turn progression |
| `TurnTerminalRecord.turn_index` | execution sequence reference | per-agent turn progression |
| `ToolExecutionRecord.turn_index` | execution sequence reference | per-agent turn progression |
| `WorkItemRecord.revision` | object revision | one WorkItem |
| `WorkingMemoryState.working_memory_revision` | state revision | one working-memory state |
| `WorkingMemoryDelta.from_revision` / `to_revision` | revision edge | one working-memory state |
| transcript `round` | parent-local order | one model turn |
| timer `fire_count` | object-local counter | one timer |
| wait `trigger_count` | object-local counter | one wait intent |
| command batch item `index` | parent-local order | one command batch |

The main gap is not that Holon has no counters. The gap is that the counter
categories are not yet named as a reusable policy.

## Candidate Additions

### First batch

#### `MessageEnvelope.message_seq`

This is the highest-value addition.

Messages are upstream of external input, internal enqueueing, queue recovery,
agent transcript construction, and event/audit correlation. A durable
`message_seq` gives Holon a stable message-ledger cursor for:

- inbox replay
- queue resume
- external ingress cursoring
- debugging and audit trails
- "continue after message N" style recovery

Suggested shape:

```rust
MessageEnvelope {
    id: MessageId,
    message_seq: u64,
    created_at: DateTime<Utc>,
    // ...
}
```

Scope:

```text
per-agent messages ledger append sequence
```

#### `TranscriptEntry.transcript_seq`

This is the second most useful ledger sequence.

Transcript entries are central to model-facing history, context assembly,
summarization, compaction, and future UI rendering. The current `round` field is
useful inside one turn, but it is not a durable transcript-ledger cursor.

Suggested shape:

```rust
TranscriptEntry {
    id: TranscriptEntryId,
    transcript_seq: u64,
    round: Option<usize>,
    // ...
}
```

Scope:

```text
per-agent transcript ledger append sequence
```

### Later candidates

#### `ToolExecutionRecord.tool_seq`

`ToolExecutionRecord` already has `id`, `turn_index`, and `created_at`, which
are enough for current debugging. A `tool_seq` becomes useful if Holon needs:

- tool-ledger pagination
- stable cross-turn tool replay
- shorter or more stable source-ref cursoring
- independent tool trace streaming

This should wait until a concrete tool-ledger API or replay surface needs it.

#### `BriefRecord.brief_seq`

Briefs are more projection-like than messages or transcript entries. They can
keep relying on `id`, `created_at`, and append order until Holon needs brief
stream pagination or memory projection cursors.

#### `TaskRecord.revision`

Tasks are mutable lifecycle objects, not append-only ledger entries. If task
state becomes externally observed or concurrently updated, `TaskRecord` should
gain:

```rust
TaskRecord {
    id: TaskId,
    revision: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    // ...
}
```

This is valuable, but it should be treated as a revision change, not a ledger
sequence change.

#### `WaitConditionRecord.revision`

Wait conditions are also mutable lifecycle objects. A revision may become useful
if Holon expands wait lifecycle transitions, retry semantics, trigger counts, or
external observation. It does not need to be first batch.

#### `WorkspaceEntry.revision`

Workspace bindings have mutable metadata, but their update rate is currently
low. Add a revision only if workspace registry updates become externally
observed, conflict-prone, or API-visible as mutable state.

## Implementation Guidance

### Sequence assignment

Each ledger append path should own sequence assignment for its ledger.

Preferred rule:

1. read the last known sequence for the ledger;
2. assign `last_seq + 1`;
3. append the record with the assigned sequence;
4. expose cursor APIs using `after_seq` only when the sequence field exists.

If the storage backend can make sequence assignment atomic, it should. For the
current local JSONL storage model, the append path should keep the logic narrow
and covered by tests. Do not spread "find max seq" logic across call sites.

### Backward compatibility

Readers should accept records that do not yet have the new field.

During incremental migration, code may derive in-memory sequence values for old
records from JSONL order when a caller needs ordered display. Derived values
must not be presented as durable stored cursors unless the migration explicitly
backfills and persists them.

If a public API introduces `after_seq`, it should define how older records are
handled before exposing the cursor as stable.

### Migration

Adoption should be incremental:

1. document the policy in this RFC;
2. implement `message_seq` only when a message-ledger cursor or queue replay
   path needs it;
3. implement `transcript_seq` when transcript replay, UI paging, or context
   assembly needs a durable transcript cursor;
4. defer `tool_seq`, `brief_seq`, and additional revisions until their owning
   surfaces need them.

Historical records do not need to be rewritten merely because the RFC exists.

### Tests

Tests for ledger sequences should assert:

- new appends receive increasing sequence numbers
- readers accept old records without sequence fields
- cursor reads skip records at or before `after_seq`
- timestamp ties do not affect sequence ordering
- sequence assignment is owned by the storage append path

Tests for revisions should assert:

- created records start at revision 1
- semantic updates increment revision by one
- no-op reads or projections do not increment revision
- stale or repeated updates behave according to the owning object contract

## Proposed First Decision

Holon should adopt the terminology and naming policy now, but should not add new
runtime fields as part of the runtime ID generation work.

The first implementation batch should be limited to:

1. `MessageEnvelope.message_seq`
2. `TranscriptEntry.transcript_seq`

Everything else should wait for a concrete surface:

- `ToolExecutionRecord.tool_seq` when tool-ledger replay or paging is needed
- `BriefRecord.brief_seq` when brief stream cursoring is needed
- `TaskRecord.revision` when task lifecycle observation or stale-update
  protection needs explicit object versions
- `WaitConditionRecord.revision` when wait lifecycle semantics become more
  complex
- `WorkspaceEntry.revision` when workspace bindings become externally mutable

This keeps the current ID RFC focused on ID shape while giving future seq and
revision work a narrow, explicit contract.
