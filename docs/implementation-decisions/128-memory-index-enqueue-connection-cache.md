# Memory index enqueue connection cache

## Choice

`RuntimeIndexOutbox` holds one long-lived shared-index connection and reuses
it for every post-write `enqueue_*_best_effort` call. A failed enqueue drops
the cached handle so the next enqueue re-opens against the on-disk index.

## Reason

Every canonical write (brief, message, task, work item, tool execution,
episode, workspace entry) enqueues one pending source into the shared memory
index. Before this change each enqueue opened a fresh SQLite connection,
replayed five pragmas, and re-ran the full `ensure_schema` statement set
(`CREATE TABLE IF NOT EXISTS` x7 plus column probes) just to execute one
upsert — on the write path of every runtime transition. Caching the handle
pays that setup once per storage instance instead of once per write. The FTS5
index write itself already runs in the daemon indexer (`refresh_memory_index_bounded`,
batch 500) outside the request path; this closes the remaining enqueue-side
per-write connection churn.

## Preserved boundary

Enqueue stays best-effort and non-transactional: failures still only warn and
mark the index dirty, and the durable runtime index outbox (written in the
same transaction as the canonical row) remains the source of truth that the
daemon consumes even when pending-source rows are lost. External deletion of
the index file while a cached handle is open is not a supported operation; if
it happens anyway, later enqueues may write to the unlinked inode until an
error surfaces, and recovery is the same as before: the durable outbox
re-projects the sources into the next on-disk index because the fresh index
has no cursors. The related `audit_events_projection_verification_insert/update`
triggers were evaluated in the same slice and are intentionally kept: they
advance the fail-closed projection-effect generation for every writer, not
just the trusted append helper, and their cost (two single-row updates and one
single-row select per audit append, inside the append transaction) is bounded.
