# Memory index uses one writer coordinator per database path

## Choice

All in-process writes to one shared memory-index SQLite file pass through a
FIFO ticket coordinator keyed by the normalized database path. Writer opens
and schema checks use the same coordinator. Pure search and status paths open
read-only, query-only connections and do not join the writer queue once the
index exists.

## Reason

The shared index is opened through multiple independent `MemoryIndex`
connections: canonical-write enqueue, background outbox and pending-source
consumption, bounded rebuild, repair, and query-adjacent refresh. A mutex owned
by one `AppStorage` instance cannot coordinate those connections, so they can
compete with each other inside one daemon and surface SQLite `BUSY` failures.
A path-keyed ticket queue removes that avoidable in-process competition while
preserving SQLite busy handling for genuinely external writers.

The coordinator records queue wait and writer-turn duration with the database
role, a path hash, and operation name. Propagated write failures also record
SQLite primary and extended error codes so remaining cross-process contention
can be distinguished from snapshot or transaction-boundary failures.

## Preserved boundary

The coordinator is not a cross-process lock and does not replace SQLite
transactions. It does not create atomicity between the memory index and the
runtime outbox, change durable acknowledgement ordering, increase
`busy_timeout`, or add unbounded retries. Transaction-mode changes, deadlines,
priority, and backlog-aware rebuild pressure remain separate follow-up work.
