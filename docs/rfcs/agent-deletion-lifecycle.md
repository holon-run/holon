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
An already loaded runtime is unloaded when the deletion request is admitted.

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
Workspace -> Index -> Home -> Finalize`. Cleanup repair starts at `Quiesce`
and uses the same idempotent terminal-safe phases. Missing AgentHome state is a
successful no-op; shared index cleanup does not depend on reopening or
recreating the deleted agent's home.

The `Index` phase removes the agent's shared memory projection, pending source
state, checkpoints, metadata, cursors, and pending runtime-index outbox rows.
The produced outbox watermark is retained as monotonic propagation evidence.

Single private parent-supervised children use either the authenticated operator
delete surface or the lifecycle-owning task's persisted
`delete_on_terminal` disposition. Explicit parent cascade uses the same
ensure-job transaction for `Active`, `Deleting`, and legacy `Deleted` children.
Public named descendants never cascade automatically.

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
