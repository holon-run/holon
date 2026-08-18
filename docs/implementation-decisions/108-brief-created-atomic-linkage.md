# Brief created-event atomic linkage (S3)

## Choice

Every Brief publication commits the Brief record, the audit event sequence
allocation, and the `brief_created` audit event in one SQLite transaction,
and stores the allocated sequence back on the record as the immutable
`created_event_seq` linkage. `persist_brief` and the work-item completion
transition are the two publication entry points, and both funnel through the
same transactional helpers (`append_brief_with_created_event` /
`link_transition_brief_created_events_tx`).

The event identity is deterministic: `stable_brief_created_event_id` hashes
the agent and brief ids, and the event `created_at` is the brief's
`created_at`. A retried publication therefore hits the audit-event
idempotency check and reuses the committed event instead of allocating a
second sequence.

The migration backfill links a historical Brief only when exactly one
candidate `brief_created` event exists for it. Zero or multiple candidates
leave the linkage `NULL` and insert a `brief_created_linkage_uncertain`
row; nothing is guessed. Brief content and timestamps are never rewritten —
only the additive linkage field changes.

## Reason

Briefs and audit events live in the same runtime DB, so a durable outbox
would add a second write path without strengthening the guarantee. The old
order (record transaction, then event transaction, then agent state) could
lose the event on a crash between the first two commits, and its random
event id made every retry append a duplicate event.

Other canonical reference families were inventoried rather than rewritten:
task, work-item, and message events already commit inside their transitions;
`agent_state_changed` is appended after the state write, which is
record-first, so an observed event still implies a readable record. A crash
there can drop the event but never inverts visibility.

## Preserved boundary

The linkage is immutable: relinking to a different sequence is a hard
conflict and rolls back the whole publication transaction. Retention may
prune Brief records independently of events, so the
`briefs.atomic-created-event.v1` verification proves linkage soundness
(every linkage resolves to exactly one matching event, no shared
sequences) rather than event-to-record existence, and unlinked historical
events remain acceptable.
