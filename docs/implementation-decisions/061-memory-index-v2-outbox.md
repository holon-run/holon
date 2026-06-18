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

The v1 `memory.sqlite3` file is intentionally ignored by v2. When v2 has not
been created yet and a v1 file is present, the runtime logs that historical v1
projection data requires an explicit rebuild/backfill instead of silently
migrating content into the new ref-discovery schema.

`index_status` reports when bounded outbox consumption hit its limit and when
individual outbox rows were skipped because their source could not be projected.
A failed row advances the outbox cursor after logging so one bad source cannot
permanently stall discovery for later refs.
