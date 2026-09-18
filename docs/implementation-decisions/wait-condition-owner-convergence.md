# Wait condition owner convergence on registration

## Decision

`upsert_wait_condition_tx` cancels stale unresolved wait rows (`active`/`triggered`)
that own the same `(agent_id, work_item_id)` key — or the same `agent_id` when
`work_item_id` is NULL — before writing a newer registration. Incoming
registrations older than the existing unresolved row still fail, so a newer live
wait is never silently discarded. The SQLite wait-owner uniqueness violation is
additionally classified as a retryable runtime error so the host rebuilds the
runtime loop (bounded restart) if a race still slips through.

## Reason

Migration 44 enforces at most one unresolved wait per owner with partial unique
indexes. A trigger settlement racing a re-registration could leave a stale
unresolved row behind; the next registration then violated the index, the whole
transition rolled back, and the failure classified as non-retryable — the agent
runtime loop exited and the host disabled automatic recovery, leaving the agent
permanently stopped (observed 2026-09-17: holon-dev2 died on
`UNIQUE constraint failed: wait_conditions.agent_id, wait_conditions.work_item_id`
and stayed silent). Reject-at-write protected the index but made the residue fatal.

## Preserved boundary

Newest-registration-wins matches the migration 44 `converge_unresolved_wait_owners`
invariant; only the storage write path converges, wake resolution semantics and
the runtime replace path are unchanged.
