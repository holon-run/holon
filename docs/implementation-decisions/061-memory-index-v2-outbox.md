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
