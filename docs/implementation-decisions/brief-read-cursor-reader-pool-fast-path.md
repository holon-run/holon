# Brief read cursor reader-pool fast path

## Decision

Serve the covered no-op case of `POST /api/agents/{agent_id}/brief-read-cursor`
and repeat `GET /api/agents/{agent_id}/brief-read-state` from the SQLite reader
pool instead of the single writer channel. Only two cases still enter the
writer: a cursor that genuinely advances, and the first-ever read/mark for a
principal/scope/agent that pins the baseline cursor row (the pre-existing
first-view semantics, asserted by
`runtime_db::tests::brief_read_cursor_counts_new_briefs_and_is_monotonic_across_reopen`).
Lookups of non-public agents answer not-found without the writer on both paths.

## Evidence

- The route carried 95.71% of all production HTTP requests in the 2026-10-30/31
  patrol windows while every request opened a write transaction whose guarded
  `UPDATE ... WHERE read_through_event_seq < ?1` changed nothing once a client
  was caught up (issue #3851).
- The no-op fast-path condition (`persisted cursor >= min(requested, head)` and
  a persisted row for the current epoch) is exactly the condition under which
  the writer path's UPDATE affects zero rows, so returned states are identical.
- `runtime_db/unread_fast_path_tests.rs` asserts zero writer-channel
  admissions (write-queue ticket deltas) for covered repeats, and unchanged
  first-touch/advance behavior; `http/unread_benchmark.rs::mark_brief_read_repeat_benchmark`
  reports the covered repeat latency with zero admissions.

## Boundary

- No API contract, schema, or cursor-semantics change; a fast-path answer may
  observe a slightly older committed snapshot than a serialized writer
  transaction would, the same staleness class as the batch read endpoint.
- The journal `http_request` span additionally carries a bounded `user_agent`
  and an `observer_principal` recorded at authentication (metadata-only), so
  per-client polling attribution no longer requires process forensics.
