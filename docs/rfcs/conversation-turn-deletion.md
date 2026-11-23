---
title: RFC: Conversation Turn Content Deletion and Unexecuted Input Withdrawal
date: 2026-11-22
status: implemented
---

# RFC: Conversation Turn Content Deletion and Unexecuted Input Withdrawal

Users need to remove mistyped, useless, or unwanted conversation content from
an agent. A conversation is not one input and one answer: `Message` is input,
`Turn` is the execution unit, `Brief` is the user-visible result, and
activities carry tool executions. One turn can batch multiple inputs, produce
multiple results, or be triggered by timers and task completions.

This RFC turns the issue-3736 specification into code-level contracts:
turn-scoped content deletion that keeps turn identity and lifecycle facts,
plus single-message withdrawal for operator input that has not been claimed
for execution.

## 1. Deletion semantics

| Operation | UI hides body | Later context stops reading it | Physical erase |
| --- | --- | --- | --- |
| Hide record | yes | no | no |
| **Delete conversation record (this RFC)** | yes | yes | no |
| Privacy purge (future, separate) | yes | yes | separate scope policy |

Deleting is not hiding, and normal deletion never claims that logs, backups,
or data already sent to model providers are erased.

## 2. Scope

### 2.1 Settled turns: keep `TurnRecord`, prune content

Only the selected turn is deleted; history is not truncated and neighboring
turns are untouched. `TurnRecord` keeps turn id, turn index, timing, run
ownership, settlement references, and a minimal deletion audit; it does not
keep original summaries, previews, or bodies.

- New nullable column `turn_records.content_deleted_at TEXT NULL`. `NULL`
  means not deleted; old rows upgrade to `NULL` naturally with no backfill.
  A non-null value records the first successful deletion time using the same
  serialization as `created_at`/`completed_at`. Deletion checks use
  `IS NOT NULL`; no parse and no deletion ledger that ordinary history
  queries must join.
- Content rows owned exclusively by the turn (identified through canonical
  associations, not time windows) are physically removed in the same
  transaction that writes the marker. Derived artifacts (episodes,
  compression state, continuation snapshots) are invalidated in that
  transaction. Search indexes and artifact files may be cleaned after commit,
  but from commit time onward no old id, search, or download may return the
  deleted content.
- Shared content, or sealed references that cannot be safely retained or
  invalidated, reject the whole request with a conflict explanation. No
  partial deletion, and no path that can still retrieve the turn's body.
- Turns that are still executing, pending admission, or holding
  not-yet-committed results cannot be deleted. Validation uses actual
  execution/settlement state, not GUI presentation. On conflict, advise
  stopping execution and retrying after settlement.
- Deletion must not break later message routing, task ownership, reference
  identity, or existing lifecycle states.

### 2.2 WorkItem: delete the conversation, keep the deliverable

The formal completion report of a completed WorkItem is an independent
deliverable. `Completed`, the completion binding, and `result_summary` are
kept; WorkItem views keep showing the report without re-reading the original
Brief. Confirmation dialogs must state that formal completion reports are
retained.

- The turn that produced the completion report can still be deleted. The
  original Brief is pruned as turn content; `result_brief_id` and the ids in
  the completion binding remain as provenance markers. They do not count as
  shared-content conflicts and do not promise the Brief stays readable.
  Direct access to the old Brief reference must not return a report copy. If
  a surface shows provenance, it shows "source turn deleted" without replay
  or expansion of the original turn. Unsettled completion reports stay under
  the active/pending-settlement conflict rules.
- `result_summary` must not be back-filled into conversations, old Brief
  references, conversation search/memory, or automatic continuation context.
  Viewing the formal deliverable through its WorkItem is unaffected. The
  exception covers only formal completion reports, never waiting reports,
  ordinary briefs, or tool output.
- `work_item_id` is an association of the same tool call, not a separate
  deliverable copy. Clearing a turn's tool executions by `(agent_id, turn_id)`
  also removes those calls from the WorkItem activity history; other turns'
  calls under the same WorkItem and all business state stay unchanged.

### 2.3 Unexecuted input withdrawal

Withdrawal applies to operator-submitted input that no execution has claimed
yet. Withdrawal competes atomically with queue claim/admission. Success means
the input can never enter model context or trigger execution. If execution
already claimed or started it, withdrawal returns an explicit conflict — the
UI never reports "withdrawn" while execution proceeds. Users are directed to
stop the execution and then delete the whole turn. Withdrawal does not delete
external events, system wakes, or other agents' messages; its permission
scope is explicit and narrow.

## 3. Storage model

One migration adds `turn_records.content_deleted_at TEXT NULL` and the
artifact cleanup queue:

```sql
CREATE TABLE IF NOT EXISTS pending_artifact_deletions (
  agent_id TEXT NOT NULL,
  relative_path TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (agent_id, relative_path)
);
```

`relative_path` is the agent-home-relative path of a turn-exclusive managed
artifact (`tool-artifacts/…`, `media/inbox/…`, `media/generated/…`). Rows are
enqueued in the deletion transaction, unlinked asynchronously after commit,
and removed only after a successful unlink; crash recovery retries.

No tombstone ledger for evidence rows: physical row removal plus
`content_deleted_at` on the turn are the authoritative state. Search index
stale documents are filtered by main-database existence checks (§6).

## 4. Deletion transaction

The transaction runs on the runtime writer (`RuntimeMutation` serialization)
and is fail-closed: any check it cannot satisfy aborts with `409` and a
body-free reason before any row is removed.

### 4.1 Exclusive content identification

Canonical association decides ownership. The candidate set is every row in
`messages`, `transcript_entries`, `tool_executions`, `model_requests`,
`model_responses`, `briefs`, `delivery_summaries`, and `artifact_metadata`
with `(agent_id, turn_id)` equal to the target turn, plus the turn's
`conversation_source_revisions` and `conversation_input_assignments` rows.

A candidate is rejected (whole request `409`, no partial deletion) when:

- the turn is not terminal (`turn_records.terminal_kind IS NULL`), or an open
  `execution_protocol_attempts` row, an unsettled `task_result_settlements`
  row, or a queue entry in `queued`/`dequeued`/`interrupted`/`interjected`
  state references the turn or its messages;
- an evidence row is shared: referenced by another turn
  (`conversation_source_revisions` with a different `turn_id`, or a turn
  payload `input_message_ids`/`tool_execution_ids`/`produced_brief_ids`
  membership of another turn), or the turn has `replay_source_turn_id`
  provenance chains that would need rewriting;
- a `scheduling_fixed_*` / `task_status_fixed_*` sealed reference, or an
  `input_evidence_eligibility` row with `mandatory = 1` that has not opted
  into `pruned` handling, names the content. (`result_brief_id` on completed
  WorkItems is explicitly exempt, §2.2.)
- the request crosses agents or names resources not owned by the target
  agent.

### 4.2 In-transaction cleanup inventory

For an accepted turn, one transaction performs all of the following:

1. **Exclusive evidence removal.** Delete the candidate rows in
   `messages`, `transcript_entries`, `tool_executions`, `model_requests`,
   `model_responses`, `briefs`, `delivery_summaries`, `artifact_metadata`,
   and the turn's `conversation_source_revisions`,
   and `conversation_input_assignments` rows that belong exclusively to the
   turn. `turn_activation_attachments` rows are **retained**: the trigger
   path re-extracts `activation_id` from the erased payload, and the
   attachment row itself carries no bodies.
2. **`turn_records.payload_json` erase.** Remove body text from
   `idle_input_batch.items[].rendered`, `live_input_batches[].items[].rendered`,
   and `terminal.reason`, keeping activation ids, item message ids,
   `result_identities`, timing, ownership, settlement references, and the
   parseable `TurnRecord` shape. The
   `turn_activation_attachments` triggers re-extract `activation_id` from the
   rewritten payload, so activation id structure must survive the rewrite.
3. **Settlement payload scrub.** In `execution_protocol_attempts.payload_json`
   clear `input_projection.rendered`; in `task_result_settlements.payload_json`
   clear `source_projection` / `reply_projection` bodies. Ids and states stay.
4. **Audit projection scrub.** For `audit_events` rows of the turn
   (`tool_executed` `input` / `text_preview` / `last_assistant_preview` /
   `exec_command_cmd` / `exec_command_display` / `exec_command_batch_items` /
   `summary` / `error`, and the `text_preview` / `last_assistant_preview` /
   `operator_message` copies on `operator_interjection_admitted`,
   `text_only_round_observed`, `turn_local_checkpoint_recorded`,
   `lineage_retry_exhausted`, `provider_failed_needs_recovery`,
   `deferred_to_fallback`, and `work_item_completion_report_candidate_promoted`),
   strip the body-bearing fields in the same transaction. The event stream
   projection (`PublicEventPayload`) treats those fields as absent for a
   deleted turn regardless of row age, so replay never leaks bodies.
   Events that only correlate by `turn_index` carry a `turn_id` so the scrub
   can attribute them; unattributable events are left intact rather than
   over-scrubbed.
5. **WorkItem provenance.** Leave `work_items.result_brief_id`,
   `result_summary`, state, and bindings untouched (§2.2).
6. **Resident/read-model state.** Set `input_evidence_current.availability =
   'pruned'` for records sourced from the turn, refresh in-memory
   `ResidentInputEvidence` / `ResidentEpisodes`, and clear affected
   `ObjectQueryCache` entries. Skip
   `conversation_read_verified`/shadow-diagnostics activity checks for the
   deleted turn.
7. **Episode and compression invalidation.** Invalidate every episode whose
   `source_turn_ids` contains the turn **or** whose
   `[start_turn_index, end_turn_index]` interval covers the turn's index
   (hard-capped and fallback episodes may omit source ids). Reset
   `AgentState.working_memory.active_episode_builder` when it references the
   turn and increment `compression_epoch`, so the next model request rebuilds
   from surviving history instead of trusting derived summaries.
8. **Marker and index outbox.** Set `content_deleted_at` (first write only;
   retries must not overwrite it), bump the turn's
   `conversation_turn_revisions.summary_revision`/`detail_revision`, emit a
   `turn_content_deleted` audit event with minimal ids through the canonical
   audit append path (allocated `event_seq`, so every client's change stream
   observes the deletion), enqueue `runtime_index_outbox` delete changes for
   every removed evidence id, and enqueue `pending_artifact_deletions` rows
   for turn-exclusive artifact paths. A managed path referenced by any other
   surviving turn's tool payload is neither enqueued nor fenced; the file
   becomes deletable with the last turn that references it.

The commit boundary is the authority: content fetched before commit but not
yet delivered must not be delivered afterwards; already-delivered content is
not recalled. Ordinary reads, event streams/replays, and model requests
issued after commit all respect the boundary.

### 4.3 Idempotency and confirmation

Re-deleting a turn whose `content_deleted_at` is already set returns success
with the existing timestamp (no rewrite). The pre-delete confirmation payload
reports input count, result count, same-turn appended inputs, shared-source
findings, and derived-content handling. Submitting a confirmed delete
re-validates server-side; a changed scope (for example a new appended input)
returns a revision conflict instead of deleting something the user never
confirmed.

## 5. Read boundaries

`content_deleted_at IS NOT NULL` is the single authoritative read boundary.
Every consumption path must honor it:

- **Conversation summary/detail** (`summary_page_in`, `active_turn_rows`,
  `turn_summary_by_id`): return a placeholder turn — identity, index, timing,
  execution outcome, revision — with empty inputs, empty `brief_ids`, no
  previews, and a `content_deleted` marker. The timeline keeps the turn's
  original position as "this turn was deleted".
- **Activities** (`activities_in`): the activity spine rows are removed with
  the turn; a deleted turn returns an empty activity list with a
  `content_deleted` coverage marker instead of dangling joins.
- **Brief reads** (`brief_in`): rows are physically gone; returns not-found.
  WorkItem views never fall back to the pruned Brief (§2.2).
- **Old references** (`message_ref`, `turn_ref`, `brief_ref`, tool output /
  content artifact refs): resolution fails closed. A deleted turn's refs
  resolve to a body-free tombstone descriptor, never content. Indexes,
  caches, and saved snapshots must not bypass the boundary.
- **Context assembly** (`context/snapshot.rs`): turns with
  `content_deleted_at` are skipped for body material; surviving history
  rebuilds the next request. Later independent turns are not rewritten.
- **Memory tools** (`MemorySearch`/`MemoryGet`, `/search`, `/memory/get`):
  main-database lookups miss (rows removed); index fallback paths must
  re-verify against the main database (§6).
- **Pending inputs** are unaffected unless individually withdrawn (§7).

## 6. Async cleanup with fail-closed reads

- **FTS search.** `MemorySearch`/`POST /search` consume the outbox
  asynchronously. Until cleanup lands (or when it fails/head-of-line blocks),
  stale FTS documents can survive. The search path filters its top-K
  candidates by main-database primary-key existence per `(source_kind,
  source_id)`; non-existent sources are dropped before ranking is returned.
  This is fail-closed: no snippet or body from a deleted turn can surface.
  Runtime databases are per agent, so the existence check arbitrates only the
  searching agent's own documents: cross-agent scope hits keep the existing
  evidence-only contract (their rows live in the origin agent's database,
  which is unreachable from the caller) and are fenced by the origin side's
  deletion transaction and its index cleanup.
- **`POST /memory/get` index fallback.** The `IndexStore` fallback for
  non-standard `RuntimeRef`s verifies the source still exists in the main
  database; otherwise it returns not-found. The fallback never returns
  deleted-turn content. The check shares the per-agent scoping above.
- **Disk artifacts.** `/tool-executions/{id}/artifacts/{idx}` rejects once
  `tool_executions` rows are gone. Generic agent-home file serving
  (`/workspaces/agent_home:<id>/files/*`, `workspace_files.rs`) and stream
  artifact recovery (`read_snapshot_stream_artifact`) block any path present
  in `pending_artifact_deletions` from commit time, before reading the disk.
  Late background results, restarts, and replays cannot resurrect content.
- **Rebuild-after-delete.** Index rebuilds and snapshot artifacts for a
  deleted turn produce nothing; rebuild never re-imports the removed rows.

If any of these cannot be made safe for a specific surface, the first release
returns `409` for the whole deletion with a body-free reason rather than a
partial deletion.

## 7. Withdrawal of unexecuted input

Withdrawal targets a message id of the target agent that is still
`queued` and not yet assigned to a turn
(`conversation_input_assignments` has no row, no turn payload references it).

- The withdrawal transaction and queue claim/admission contend through the
  same writer serialization: withdrawal succeeds only by transitioning the
  queue entry from `queued` to a terminal `withdrawn` state and deleting the
  exclusive `messages` row; claim succeeds only from `queued`. Exactly one
  wins.
- `QueueEntryRecord` carries metadata only. The terminal `withdrawn` state on
  the queue entry (a minimal body-free audit marker) makes retries idempotent:
  a retry finds the marker and returns success with the original outcome
  instead of `404` from the missing `messages` row.
- Already-claimed (`dequeued`/`interjected`/…) input returns a conflict that
  names the owning execution, with guidance to stop it and delete the turn
  after settlement.
- Withdrawal applies to operator-submitted input. External events, system
  wakes, timer fires, and other agents' messages are out of scope and are
  rejected with a typed error.

## 8. HTTP contract

Operator-authenticated routes on the daemon; target agent and resource
ownership are validated, and no route performs "delete arbitrary rows".

| Route | Purpose |
| --- | --- |
| `GET /api/agents/{agent_id}/turns/{turn_id}/deletion-preview` | Confirmation impact scope: input/result counts, appended inputs, shared-source and derived handling notes, current revision. Body-free on errors. |
| `DELETE /api/agents/{agent_id}/turns/{turn_id}/content` | Delete the turn's conversation content. Optional `expected_revision` re-validation. Idempotent. |
| `POST /api/agents/{agent_id}/messages/{message_id}/withdraw` | Withdraw one unexecuted operator input. Idempotent. |

Success responses carry target ids, deletion/withdrawal status, timestamps,
and updated revisions; they never carry bodies. Typed failures distinguish
not found, forbidden, executing/claimed, revision conflict, shared content,
sealed reference, and cross-agent access — all body-free. Deleted-reference
responses include only minimal identity and a `content_deleted` marker.

## 9. Event contract

The conversation change stream carries a minimal
`turn_content_deleted` message: agent id, turn id, turn key, deletion
timestamp, and the bumped turn revision. The event is appended through the
canonical audit path with an allocated `event_seq` (advancing the audit
sequence head and notifying the writer queue), so cursor-based incremental
reads deliver it as the deleted turn's placeholder `turn_summary_upsert`.
Clients use it to invalidate
summary/detail/Brief caches and replace the turn in place with the
placeholder. Raw event facts stay durable; ordinary content interfaces never
replay deleted bodies. Withdrawal emits `input_withdrawn` with the message
id; incremental reads surface it as an `operator_remove` for that input.
Old-version events or responses must never overwrite the
placeholder with stale bodies.

## 10. Client contract (first release)

- Timeline updates reuse `turn_summary_upsert`: the deleted turn is replaced
  in place with the placeholder carrying the new revision.
- The client that executed the deletion clears its local caches for the
  turn's list entry, detail, Brief, and snapshot immediately on success.
  Other online clients clear after receiving the versioned update.
  Cross-client/offline cache invalidation protocols are explicitly out of
  scope for this release: reconnect aligns with the server before rendering
  cached bodies; nothing promises instant cleanup of disconnected offline
  copies.
- Pagination, refresh, and restart render the placeholder, never bodies.
  Deleted turns show only index, time, and placeholder text, cannot be
  expanded or replayed, and diagnostic surfaces reuse the same body-free
  placeholder.
- UI offers "delete conversation record" on finished turn groups and
  "withdraw" on unexecuted inputs. First release ships the server contract
  and placeholder rendering; the web client's action entry points land in a
  follow-up, and API clients may drive the endpoints directly.
  Confirmation shows the real impact scope
  (multi-input/multi-result turns included), states that later context stops
  reading the deleted content, that formal WorkItem completion reports are
  retained, that physical erase is not guaranteed, and that performed actions
  are not undone. Running turns explain why deletion is unavailable and link
  to stop; stop and delete stay separate actions.

## 11. Test matrix

Each acceptance item in the issue maps to at least one test:

1. Three turns A/B/C, delete B: timeline keeps A and C intact with a
   placeholder B; B is not expandable or replayable; ownership and settlement
   references survive.
2. Upgrade: old rows have `content_deleted_at NULL` and stay readable;
   deletion persists in the same transaction, survives restart, and idempotent
   retries keep the first timestamp.
3. Multi-input/multi-brief/tool-call turn: preview counts match; exclusive
   rows are removed; no readable orphan bodies remain.
4. Same WorkItem across A/B/C: deleting B removes only B's tool calls from
   conversation and WorkItem history; A/C and WorkItem state unchanged.
5. Completion-report turn: deletion succeeds; WorkItem stays `Completed` and
   re-renders its report after restart; the original Brief, placeholders, old
   links, search/memory, and continuation context never return the report;
   provenance surfaces mark deletion.
6. Read boundary sweep: summary, detail, search, memory tools, direct old
   refs, and content artifacts cannot return deleted bodies.
7. Episode/compression invalidation: next model request contains neither the
   deleted sources nor unsafe derived content.
8. Delayed/failed index and disk cleanup: deletion still succeeds; from
   commit, search, old refs, generic file download, replay, and new model
   requests return no body (including content read before commit but
   delivered after); restarts and late background results do not resurrect;
   A/C unaffected.
9. Client cache: initiator clears immediately; another online client replaces
   in place on the versioned update; stale responses, pagination, and
   reconnect never restore bodies; reconnect aligns before reusing cache.
10. Withdrawal/claim race: exactly one outcome wins; retry idempotent.
11. Rejection paths: active execution, pending settlement, scope change after
    confirmation, shared content, unsafe sealed references, cross-agent
    resources, unauthorized requests — typed errors with tests, no partial
    deletion.
12. Deletion does not cancel timers/tasks, mutate WorkItem lifecycle, roll
    back external operations, or break later routing.
13. GUI, HTTP/SDK contract, context assembly, and search/reference
    integration tests plus user documentation cover the semantics.

## 12. Non-goals

- Deleting the conversation that created a timer does not cancel the timer;
  WorkItems, background tasks, and timers keep their own lifecycles.
- Deleting a development conversation does not roll back code; deleting a
  send conversation does not unsend.
- "Delete this turn and everything after" is branching/rollback, not this
  feature.
- Precise deletion/sanitization of a single executed message, restoring
  deleted bodies, physical purge, and cross-system copy governance are future
  designs.

## 13. Relationship to other RFCs

- `conversation-read-model.md` keeps its projection contracts; this RFC adds
   the deletion boundary the projection must honor and the placeholder shape.
- `runtime-ref-resolution-and-memory-get.md` governs reference resolution;
   this RFC adds fail-closed behavior for deleted sources and main-database
   verification for index fallbacks.
- `long-lived-context-memory.md` governs episodes and compression; this RFC
   defines invalidation triggers (`availability='pruned'`, interval matching,
   `compression_epoch` bump, builder reset) without changing its retention
   model.

Implementation reference:
`crates/holon-domain/src/conversation.rs`,
`crates/holon-runtime/src/runtime_db/conversation.rs`,
`crates/holon-storage/src/conversation.rs`,
`crates/holon-storage/src/migrations.rs`,
`crates/holon-runtime/src/runtime_db/retention.rs` (exclusive/shared
identification pattern),
`crates/holon-runtime/src/runtime_db/agent_deletion_cleanup.rs` (guarded
purge pattern), `crates/holon-runtime/src/memory/index.rs`,
`crates/holon-http-api/src/http/workspace_files.rs`.
