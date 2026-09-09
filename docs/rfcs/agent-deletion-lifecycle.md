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

Deletion is an authenticated operator control-plane operation. It creates a
durable, idempotent deletion job before cleanup begins.

## Phase 0–1 Contract

Agent identity is canonical in `agent_identities.payload_json`, with projected
columns for queries. Legacy `archived` identity payloads decode as `Deleted`;
the historical SQLite `archived_at` column remains a compatibility projection
for the Rust `deleted_at` field.

The first deletion transaction:

1. verifies that the identity exists and its revision is current;
2. rejects the configured default agent and identities that are not public and
   self-owned;
3. changes identity status from `Active` to `Deleting`;
4. inserts one durable `agent_deletion_jobs` record;
5. returns the existing job for repeated delete requests.

After the fence commits, runtime bootstrap, ingress, wake, prompt, enqueue, and
control paths must not return or create a runnable runtime for that identity.
An already loaded runtime is unloaded when the deletion request is admitted.

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
- Release only ever happens through an operator-triggered deletion;
  agents cannot delete themselves or each other. External stale references
  to a recreated id resolve to the new incarnation — the accepted residual
  risk, mitigated by the incarnation counter and audit boundary.

## Cleanup Boundary

Phase 0–1 establishes the fence, durable job, status API, and restart-safe
record. Later phases advance the job through runtime, ingress, scheduler,
workspace, index, AgentHome, and finalization cleanup. Cleanup must be
reentrant and fail closed on dirty, locked, or occupied managed worktrees.

Private-child cleanup remains on the existing parent-supervised path until it
is migrated to the unified cleanup engine. Public named descendants never
cascade automatically; private children cascade only when explicitly
requested.
