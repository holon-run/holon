# Memory index writer admission gives foreground work strict priority

## Choice

The per-path memory-index writer coordinator maintains separate FIFO queues for
foreground and maintenance writes. When no writer turn is active, it admits the
oldest foreground waiter if one exists; otherwise it admits the oldest
maintenance waiter.

Foreground covers schema/open, enqueue, outbox and pending-source consumption,
repair, and ordinary document writes. Maintenance covers the persisted rebuild
lifecycle: start, cursor advance, scan, prune, phase updates, and finalize.
Callers pass the class explicitly rather than deriving it from an operation
name.

## Reason

A single FIFO queue let a backlog of short rebuild transactions consume the
shared five-second transaction deadline before later foreground writes could
start. Fixed sleeps in rebuild callers cannot protect all entry points and do
not let newly arrived foreground work overtake maintenance that has not begun.

Strict foreground priority bounds an admitted foreground write behind only the
active writer turn and older foreground waiters. FIFO ordering remains intact
within each class, and only one in-process writer turn remains active for a
database path.

## Preserved boundaries

Maintenance may pause under sustained foreground load. This is intentional
backpressure: rebuild jobs, cursors, phases, and watermarks are persisted, so a
later bounded rebuild slice resumes the same work after foreground pressure
falls. This decision does not add weighted fairness or force maintenance turns
through foreground traffic.

Queue timeout, finite SQLite retry, transaction rollback, and panic/unwind
release remain bounded by the existing transaction deadline. Diagnostics expose
total and per-class queue depth, per-class timeout counts, and bounded
operation-specific queue-wait metrics without external labels.

The coordinator remains an in-process admission boundary keyed by normalized
database path. SQLite locking continues to arbitrate other processes. This
decision extends
[`132-memory-index-per-path-writer-coordinator.md`](132-memory-index-per-path-writer-coordinator.md)
and does not change cross-process locking or the recoverable rebuild format.
