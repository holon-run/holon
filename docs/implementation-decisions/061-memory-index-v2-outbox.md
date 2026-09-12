# Memory Index v2 Uses Runtime Outbox

Runtime writes that affect `MemorySearch` discovery now record lightweight
index changes in `runtime_index_outbox` inside the same `runtime.sqlite`
transaction as the canonical record.

The memory index is a rebuildable ref discovery projection, not a content store.
`memory.v2.sqlite3` stores bounded searchable text, snippets, provenance, and
opaque `source_ref` values. Exact content remains owned by runtime evidence,
state tables, or governed memory files and is read through `MemoryGet`.

`MemorySearch` must not synchronously full-rebuild the index. Search may consume
a bounded number of outbox rows and return stale or empty results with index
status when the projection is missing or behind. Full rebuild/backfill is an
explicit maintenance action, not a model tool side effect.

Online rebuild requests use the same maintenance path as incremental refresh:
they enqueue a rebuild intent in `memory_index_pending_sources`, mark the index
stale, and let the background indexer consume the intent before bounded outbox
refresh. The background indexer discovers agents from both the runtime outbox
and the shared pending-source queue, so a rebuild intent does not require a new
runtime write to become runnable. The CLI's default `memory-index rebuild`
behavior submits that intent; `--offline` remains the explicit foreground repair
path for stopped-service or operator-controlled maintenance.

The v1 `memory.sqlite3` file is intentionally ignored by v2. When v2 has not
been created yet and a v1 file is present, the runtime logs that historical v1
projection data requires an explicit rebuild/backfill instead of silently
migrating content into the new ref-discovery schema.

`index_status` reports stale/incomplete results when the current projection lacks
any required full-backfill checkpoint, even if its runtime outbox cursor is
caught up. It also reports when bounded outbox consumption hit its limit
(`consumption_was_limited`) and how many rows the current consume attempt
failed to apply (`skipped_error_count`).

Outbox consumption is at-least-once with contiguous-prefix acknowledgment: the
applied cursor advances only inside the memory index transaction that projects
a row, a failed row and everything after it stay in the runtime outbox as a
durable retry queue, and the daemon backs off a persistently failing agent.
Rows are deleted only after the cursor has covered them, so a projection
failure can delay discovery but never silently drop refs. `index_status`
additionally tracks a monotonic per-agent produced watermark that survives row
GC, and counts pending rows only above the applied cursor: rows at or below it
are acknowledged garbage awaiting GC, including rows a full rebuild jumped the
cursor past.
