# Polled HTTP DB reads run off tokio workers under one deadline

Date: 2026-10-29. Follow-up to the 2026-10-29 stall RCA: during /events
polling all 16 tokio workers were blocked inside synchronous SQLite reads,
making the process appear frozen for 8-22s.

## Choice and preserved boundary

Polled HTTP read handlers (paged `/events`, SSE stream open, message batch,
app events, task list/get, operator briefs/transcript) run their storage reads
through `AppStorage::read_block`, which moves the whole read block onto
`tokio::task::spawn_blocking` under one absolute read deadline
(`HTTP_READ_BLOCK_BUDGET`, 10s). A slow read then costs one request — the
deadline cancels SQL through the existing scoped-read progress hook and
surfaces as the existing retryable runtime-db error / 503 — instead of
pinning a tokio worker.

Preserved boundaries:

- Reader pool capacity and the 250ms acquisition budget are unchanged; the
  bridge uses tokio's blocking pool, not a new connection pool.
- Explicitly scoped read handles still win over the ambient block deadline.
- `with_read` stays synchronous-only; `read_block` is its async-entry wrapper.
  `#[track_caller]` is a no-op on async functions, so async reads report an
  unknown call site.

## Slow read / slow SQL trace

`HOLON_SLOW_DB_READ_MS` (default 1000ms, `0` disables) gates two
`holon::performance` warnings:

- whole read blocks: call site (file:line), checkout vs execution split, and
  HTTP read label such as `http.events.page`;
- single SQLite statements above the same threshold: collapsed SQL text
  truncated to 512 chars, via `rusqlite` `Connection::profile` installed on
  pooled reader connections only, so writer and migration connections stay
  unprofiled.

`Connection::profile` callbacks carry no user context, so the SQL-to-call-site
link travels through a thread-local read context that the blocking read
bridge sets (label) and `with_read`'s `#[track_caller]` origin enriches.
