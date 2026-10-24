---
title: RFC: Agent Deletion Lifecycle
date: 2026-07-24
status: draft
---

# RFC: Agent Deletion Lifecycle

## Summary

Holon separates reversible execution control from irreversible identity
deletion:

- `stop` / `start` change `AgentStatus` and remain reversible;
- `delete` transitions identity `Active -> Deleting -> Deleted`;
- `purge` is a future evidence-erasure contract and is not part of delete.

Deletion is normally an authenticated operator control-plane operation. A
parent-supervised task may also persist an explicit lifecycle disposition that
admits deletion when its one-shot child becomes terminal. Both paths create the
same durable, idempotent deletion job before cleanup begins.

The same operation repairs legacy terminal private children that were archived
without a deletion job. A repair job records `mode=cleanup_repair`, keeps the
identity `Deleted`, and starts at `Quiesce`; normal jobs record `mode=delete`
and retain the `Active -> Deleting` fence.

## Phase 0–1 Contract

Agent identity is canonical in `agent_identities.payload_json`, with projected
columns for queries. Legacy `archived` identity payloads decode as `Deleted`;
the historical SQLite `archived_at` column remains a compatibility projection
for the Rust `deleted_at` field.

Database upgrade also canonicalizes every legacy `archived` identity to
`deleted` and retires its identity reservation in the same transaction,
including tombstones without a completed deletion job. Verification remains
alias-aware during compatibility reads, so historical spelling cannot disable
observer-sync capabilities before the idempotent reconcile completes.

The deletion transaction verifies the current identity revision and then:

1. rejects the configured default agent and identities that are neither public
   self-owned agents nor private parent-supervised child agents;
2. returns an existing deletion job for repeated requests;
3. for `Active`, atomically changes identity status to `Deleting` and inserts a
   normal job starting at `Fence`;
4. for `Deleting` without a job, inserts a recovery job starting at `Fence`
   without changing identity revision again;
5. for `Deleted` without a job, inserts a `cleanup_repair` job starting at
   `Quiesce` without reopening the identity;
6. replaces a legacy completed `delete` job once with `cleanup_repair`, because
   completion under the older contract is not evidence that shared index and
   outbox cleanup ran. A completed repair job is returned idempotently.

`AgentDeletionJob.mode` is backward-compatible: old payloads without the field
decode as `delete`. A `cleanup_repair` job is valid only while the canonical
identity remains `Deleted`; it never synthesizes `Deleted -> Deleting`.
Concurrent requests are serialized by the identity revision and the unique
per-agent deletion-job row. Existing actionable jobs are returned before
revision validation so retries carrying the original admission revision remain
idempotent; revision validation fences only job creation or replacement.

After the fence commits, runtime bootstrap, ingress, wake, prompt, enqueue, and
control paths must not return or create a runnable runtime for that identity.
Destructive deletion unloads an already loaded runtime at admission. Safe cleanup
closes execution admission and cooperatively joins the retiring instance before
resource removal. A cross-entry replay follows the already admitted job's policy;
an operator replay of safe cleanup must not force-abort that instance.

## Supervised Child Terminal Disposition

The task recovery contract persists an `AgentLifecycleDisposition`:

- legacy and current `ChildAgentTask` records default to
  `delete_on_terminal`;
- `ActorInvocation` records created by `InvokeAgent`, including
  `InvokeAgent(new_subagent)`, default to `retain`.

Terminal deletion admission is allowed only when the target resolves to the
canonical `Child + Private + ParentSupervised + Ephemeral` shape, its lifecycle
is `supervision_attached`, and its lineage, supervisor, and delegated task all
match the terminal parent task. Agent names and legacy name prefixes are not
admission evidence.

For `delete_on_terminal`, the terminal task/result settlement, the
`Active -> Deleting` identity fence, and deletion-job creation commit in one
runtime-database transaction. The parent then wakes the deletion coordinator;
a crash or wake failure after commit cannot lose the durable job. Replayed
terminal notifications return the existing job, including a completed job,
and never promote normal terminal cleanup to `cleanup_repair`.

For `retain`, task completion does not create or wake a deletion job. The child
identity remains available for another invocation, including after runtime
restart. This preserves the reusable `InvokeAgent(new_subagent)` contract.

Normal supervised terminal cleanup never uses the archive-only compatibility
path. Repairing already-deleted legacy residue remains an explicit operator or
maintenance admission and is outside terminal-task cleanup.

## Legacy Residue Maintenance Scan

The daemon deletion coordinator also owns a bounded maintenance scan for
upgrade residue. The scanner walks identities in stable `agent_id` order with
a durable keyset cursor stored in runtime metadata. Each pass reads at most one
bounded batch, advances the cursor even when a candidate is ambiguous, and
yields before continuing. Reopening the runtime resumes after the persisted
cursor; reaching the end clears it so a later safety sweep starts a new cycle.

The scanner may create a job only for these proven shapes:

1. an `Active` private parent-supervised ephemeral child whose resolved
   canonical lineage, supervision, durability, lifecycle attachment, and
   terminal one-shot child task all agree; this creates the normal `delete`
   generation through the same terminal-child admission used by live task
   settlement;
2. a `Deleted` identity without a job; this creates `cleanup_repair`;
3. a `Deleted` identity whose completed legacy job has `mode=delete`; this
   replaces that old generation once with `cleanup_repair`, because legacy
   completion is not proof that current outbox and shared-index phases ran.

Existing `Pending`, `Running`, or `RetryableFailed` jobs remain coordinator
owned and are never duplicated. A completed `cleanup_repair` generation is
terminal and is not replaced on every scan.

Missing or conflicting relations, task ownership, task kind, or lifecycle
evidence never authorize deletion. The scanner emits a structured
`legacy_deletion_repair_ambiguous` audit event keyed by the identity
incarnation, revision, reason code, and observed facts, so unchanged ambiguity
deduplicates across repeated scans. Names such as `tmp_child_*` are never
evidence. Terminal `ActorInvocation` tasks with `retain`, including reusable
`InvokeAgent(new_subagent)` children, remain active.

## Coordinator Ownership and Retry

One daemon-owned coordinator is the only normal executor of deletion jobs.
Deletion admission emits a coalesced wake; it never starts a detached full
sweep. The coordinator processes a bounded batch, yields between full batches,
and then checks for newly admitted work again.

Persisted status has one owner interpretation:

- `Pending` is eligible for the coordinator;
- `Running` is owned by the live process-local coordinator and is excluded from
  ordinary due queries;
- `RetryableFailed` is eligible only when `next_attempt_at` is absent for
  legacy compatibility or has elapsed;
- `Completed` is terminal.

At daemon startup, before ordinary draining begins, persisted `Running` jobs
are atomically reset to due `RetryableFailed` jobs. Recovery preserves phase,
attempt count, and last error; normal sweeps never infer that a `Running` job
is stale.

Transient phase failure persists a capped exponential retry deadline with
deterministic per-job jitter. The coordinator wakes for the earliest deadline
or a periodic safety sweep. This bounds lock-error amplification while keeping
retry state crash-recoverable. A future multi-process runtime sharing one
database would require explicit owner leases and fenced updates; that contract
is not implied by the current single-daemon model.

## HTTP Contract

The authenticated operator control plane exposes:

- `DELETE /api/control/agents/{agent_id}` to create or return the deletion job;
- `GET /api/control/agents/{agent_id}/delete-status` to read identity and job.

Status semantics are:

- unknown identity: `404 Not Found`;
- `Deleting`: `409 Conflict` on ordinary agent surfaces;
- `Deleted`: `410 Gone` on ordinary agent surfaces;
- forbidden delete target, including the configured default agent: `409
  Conflict`.

Normal public lists include only `Active` identities.

## ID Release and Reincarnation

An agent id is released for creation again once its deletion job reaches
`Completed`. Re-creating a released id never revives the old agent: it starts
a new incarnation with a fresh AgentHome bootstrap and no inherited
WorkItems, tasks, waits, timers, triggers, queue entries, occupancy, or
workspace bindings.

Retained turns remain historical evidence keyed by agent id. Their `turn_index`
high-water mark survives recreation without restoring the previous runtime state.
The first new canonical turn allocates above that mark; creation itself does not
restore an old AgentState or make old execution bindings valid.

- The identity record carries a monotonic `incarnation` counter (`1` for the
  original identity, `+1` per recreation) and continues its `revision`
  counter so stale observers cannot write back.
- The recreation runs in one transaction that flips the identity reservation
  from `retired` back to `active` — the only sanctioned `retired -> active`
  transition — and appends an `agent_recreated` audit boundary event
  (`agent_id`, `incarnation`, `previous_deleted_at`, `deletion_id`,
  `requested_by`).
- Historical evidence (deletion jobs, audit events) is retained and stays
  distinguishable from the new incarnation via the incarnation counter and
  the `agent_recreated` boundary.
- If the latest deletion job is not `Completed` (in-flight, pending, or
  `retryable_failed`), create fails closed with `409 Conflict`,
  `deletion_incomplete`, including job status/phase/last_error details.
- Release only happens after a canonical deletion job completes. Admission is
  either operator-authenticated or the persisted terminal disposition of the
  lifecycle-owning supervision task; agents cannot otherwise delete themselves
  or peers. External stale references to a recreated id resolve to the new
  incarnation — the accepted residual risk, mitigated by the incarnation
  counter and audit boundary.

## Cleanup Boundary

Normal deletion advances through `Fence -> Quiesce -> Ingress -> Scheduler ->
Workspace -> Archive -> Index -> Home -> Finalize`. Cleanup repair starts at `Quiesce`
and uses the same idempotent terminal-safe phases. Missing AgentHome state is a
successful no-op; shared index cleanup does not depend on reopening or
recreating the deleted agent's home.

The `Scheduler` phase uses a deletion-only WorkItem focus transaction. It
releases durable focus and turn/execution attribution before changing a
WorkItem out of `Open`, and atomically cancels continuation returns, writes
completion intent/result evidence, audits, and index outbox changes. The
owned/open focus triggers remain enforced. Reentry can resume an existing
`Completing` item without duplicating its completion result.

That same transaction interrupts any remaining canonical execution attempts
with outcome evidence and retires all execution WorkItem demand. Incomplete
sources become `Paused(agent_deleted)` until their result is bound; completed
sources become `Terminal` with that brief. Legacy completed sources without a
brief retain existing terminal evidence, or remain non-runnable without
inventing a brief. An agent with no incomplete WorkItems still releases its
focus/attribution and retires stale execution demand. Historical attempts and
outcomes are retained until private-history cleanup; a missing execution
partition is not bootstrapped.
These terminal-safe writes require an admitted deleting/deleted identity and
do not reopen parent-cleanup admission or relax ordinary protocol write fences.

The `Index` phase removes the agent's shared memory projection, pending source
state, checkpoints, metadata, cursors, and pending runtime-index outbox rows.
The produced outbox watermark is retained as monotonic propagation evidence.

Shared-index cleanup commits bounded maintenance batches and releases the
writer coordinator between them. It uses the FTS rowid map for healthy
documents, followed by a bounded-candidate rowid sweep for residual, duplicate,
or incompletely mapped FTS rows. Agent ids cannot contain `:`, so the
`agent_id:` document-key prefix identifies only that agent during the sweep.
Other large projection tables are also paged; small lifecycle metadata is
removed last. A failed `Index` phase can repeat these idempotent batches without
reopening the deleted AgentHome. Partial derived cleanup does not complete the
deletion job, release the id for reincarnation, or relax memory-index admission.

Single private parent-supervised children use either the authenticated operator
delete surface or the lifecycle-owning task's persisted
`delete_on_terminal` disposition. Explicit parent cascade uses the same
ensure-job transaction for `Active`, `Deleting`, and legacy `Deleted` children.
Public named descendants never cascade automatically.

## Private history and clean-slate recreation (2026-10-23)

`stop` preserves resumable data. `delete` fences admission, stops the runtime and
settles its execution before removing agent-owned private history. Quiescence
interrupts open turns with an explicit no-Brief reason and aborts nonterminal
queue/attempt/claim/activation evidence; it never makes an old input replayable
or rewrites a successful terminal result. Parent cleanup retains its existing
active-work and protected-resource checks.

The settlement transaction also cancels both active and triggered waits, and
closes destructive-operation recovery ownership as `owner_closed`. This terminal
state does not assert that an externally dispatched command was cancelled or its
target verified; deletion neither executes verification commands nor replays
external effects. Previously verified operations remain verified. Settlement
proof is valid only while no unresolved wait or nonterminal owned operation
remains, so a legacy proof that omitted these families restarts at the
cancellation gate rather than retrying `Index` indefinitely.
Before purging operation payloads, the same transaction retains a global
dispatch-fence tombstone keyed only by a hash of `operation_id`. It contains no
agent, turn, command, verification target or result. Both operation admission
paths reject reuse of a tombstoned id, including after recreation, so deleting
private ownership evidence cannot authorize replay of an external side effect.

Deletion removes private conversation, tools, tasks, waits, timers, WorkItems,
runtime traces and their owned derived projections, as well as the home allowed
by the selected deletion policy. Shared repositories, other agents' data and
protected or ambiguously owned resources are not deletion authority. Keep only
identity incarnation/revision tombstones, necessary deletion fencing and
content-free cleanup evidence. Database cleanup is transactional; filesystem
cleanup remains a separate retryable job phase. A failed phase cannot mark the
job complete or allow a replacement.

### Historical references and consumer cancellation (2026-10-24)

An ID in another agent's tool, message, audit or task history does not retain
ownership of the referenced private row. Export and purge select rows by their
explicit owner; they never traverse foreign JSON to discover consumers or rewrite
another agent's history. Dereferencing purged history may return unavailable.
Unknown owner schemas and jointly owned resources still fail closed. Global
search-index orphan auditing is separate maintenance, not a deletion preflight.

The durable identity fence is also a cancellation fact for pending invocations.
Consumers resolve their persisted target incarnation before reading private source
or execution evidence. A same-generation `Deleting`/`Deleted` identity, or a newer
incarnation, ends an unfinished invocation as `Cancelled` with reason
`agent_deleted`. This follows the existing atomic task/result/wait settlement
protocol and wakes the caller. It never creates a replacement, resends the old
request, or retries an external effect. Missing identity, an impossible future
generation and ambiguous legacy bindings are not deletion evidence.

This is lazy consumer settlement: deletion need not activate parents or find
historical references. A running monitor observes the fence; an unloaded caller
settles from the same durable fact during recovery. Task-owned worktrees retained
by cancellation remain explicitly owned by the parent task, marked
`retained_for_review` with reason `agent_deleted`; physical cleanup uses the
protected workspace lease and is not part of lazy result settlement. This also
applies when stopping a task archives its child. Ordinary successful/failed task
cleanup first proves the exact supervised invocation terminal, closes its idle
runtime admission, joins its exit and releases its workspace occupancy before
acquiring the physical cleanup lease. Legacy task worktrees without root-level ownership metadata require
an exact parent-owned worktree task artifact and its dedicated task branch;
unknown, foreign and shared roots remain protected. Parent task results that
already committed remain immutable. A losing concurrent terminal publisher exits
when it observes that committed result. Accepted message waits persist their
target incarnation, delivery and reply cursor in the admission transaction, so a
crash before monitor startup does not erase their generation binding.

Deletion completion still means private cleanup completed. It does not mean all
caller projections have refreshed. Identity tombstones and incarnation fences
survive purge; ordinary historical links may remain unavailable indefinitely.
Parent cleanup retains its rule that accepted active work cannot be cancelled to
make a child eligible. Operator deletion retains explicit cancellation semantics.

Recovery requires same-incarnation execution-settlement proof before resuming
past `Quiesce`, and purge proof before resuming past `Index`. A legacy job
without that proof restarts at its cancellation gate. A job with valid proof
keeps its persisted phase, so retrying home cleanup cannot recreate private
audit or execution rows after purge.

Historical retention is explicit export, not an implicit backup or a data source
for a replacement. An export failure must not proceed to destructive cleanup.
The minimal operator entry point is `holon agent delete ID --archive`: admission
persists a unique archive destination under the runtime's `archives/` directory,
and the retryable `Archive` phase exports the fenced database snapshot and
agent-home without following symlinks. It seals and validates the archive before
`Index` removes any private history. The destination cannot be changed on a
retry; enabling archival after deletion started is rejected. Archives are
explicit user-retained files, never replacement context or implicit backups.
Legacy `Deleted` identities whose old completed jobs retained private history
must finish cleanup before the recreation writer transaction admits them.
Already-`Active` legacy identities are not repaired by guessing record ages or
mutating read APIs. Late callbacks, old cursors and bootstrap retries remain
incarnation-fenced.

## Memory Index Admission

Memory-index candidates are the union of runtime outbox rows, pending source
state, produced-watermark backfill candidates, and self-heal discovery.
Immediately before refresh or rebuild dispatch, the daemon reads the canonical
identity from `agent_identities` while holding the per-agent bootstrap fence.
Only `Active` identities proceed; active private children remain eligible.
Unknown, `Deleting`, `Deleted`, or undecodable identities fail closed for that
round.

Operator deletion, parent cascade, and terminal supervised cleanup acquire the
same fence before changing a child identity, preventing a refresh from crossing
the deletion admission boundary.

## Parent-requested cleanup

A supervising parent may directly request deletion of its ephemeral,
supervision-attached private child. Authorization follows current supervision
and capability policy, and the request identifies the child's incarnation.
The `parent_cleanup` deletion mode fences new execution and references only
after protected work and resources pass admission. It cooperatively joins owned
runtime work and never cancels legitimate work to make deletion possible.
Protected artifacts, descendants, dirty/shared/occupied or foreign worktrees
block this mode.
Retries preserve the durable job and parent result. Missing parent/task evidence
at startup is not authorization to archive a child or remove its home.

### Shared deletion core and explicit policies

Tool and operator CLI requests use the same resource classification and deletion
phases. Authorization selects an explicit policy, not a separate filesystem
implementation: `parent_cleanup` preserves accepted work and protected resources;
operator `delete` (and explicit legacy repair) may cancel work and discard user
home data. The separate `delete_on_terminal` child lifecycle also admits
`mode=delete` and uses that destructive policy; operator deletion is not its
exclusive source. Persisted job modes remain compatible.

Both policies reject symlinked homes, paths outside the runtime agents root,
and dirty/shared/occupied or foreign worktrees. Worktree cleanup requires an
exclusively owned, runtime-created, clean registered worktree and successful
`git worktree remove`; it never force-removes or falls back to directory deletion.
The operator policy releases its stopped agent's occupancies, not other agents'.

Home classification treats identity configuration and runtime state
(`.holon/`, `memory/`, `skills/`, `work-items/`) and command/tool output
(`task-output/`, `tool-artifacts/`) as disposable runtime-owned namespaces.
The safe policy requires their expected file/directory types, not symlinks.
Historical terminal command output references do not retain those bytes.
Registered protected artifacts and user deliverables, including nonempty
`work/`, `notes/`, `media/`, `apps/`, or unknown files, still block safe
cleanup; explicit operator deletion authorizes their removal. A real `tmp/`
directory is documented disposable scratch space: the safe policy removes it
like runtime-owned output, while a file or symlink named `tmp` is rejected as
an unknown artifact.

Home validation applies again at physical removal and when resuming from
rename-to-trash. An in-progress deletion job owns its trash; the orphan sweep
cannot bypass its policy or a failed Home phase. Host deletion admission, Home,
and the orphan sweep share an ownership fence. Directory enumeration is not
ownership evidence: each sweep removal rechecks current identities and both
full-ID and legacy aliases while holding that fence. Parent task result payloads
remain readable after the child's runtime-owned files and identity are removed.
Trash names append `.deleting_trash` to the complete ID, including dots. Legacy
extension-replaced trash is recovered only with unambiguous identity ownership;
aliases shared by registered identities fail closed.
Both Home and the orphan sweep check cross-identity full/legacy aliases,
including when the current home is absent or the conflicting directory has
not yet been created. Operator authorization for one identity never authorizes
removing another identity's home or trash.

An idempotent request from either entry point returns the already admitted job
and its policy; it does not silently upgrade safe cleanup to destructive deletion
or restart phases. Parent replays still require current supervision/incarnation
authorization. Protected resources must be resolved before retrying that job.
There is no policy-upgrade API for an admitted job: even an operator `delete`
retry after a safe job fails in Home preserves `PreserveProtectedResources`.
The operator must first move protected files out of that agent's home or trash,
then retry the existing job. A parent replay likewise cannot downgrade an
operator-admitted `DeleteAgentData` job.

See [subagent residency and reclamation](./subagent-residency-and-reclamation.md).
