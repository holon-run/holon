# Queue latest WorkItem index

Use an admitted append-only Vec builder and an immutable sorted Vec index for
the queue's latest WorkItems. A HashMap does not expose its allocation layout,
so estimating its bucket growth would couple the admission contract to private
standard-library implementation details.

Admit geometric slot growth before reserving, move the existing key/record
material, and sort by key plus reverse source ordinal before deduplication.
This preserves last-source-wins without record clones or stable-sort scratch,
with O(n log n) construction and O(log n) borrowed lookup. Duplicate candidates
and spare capacity remain charged for the original phase lifetime. The existing
key/record admission order, queue eligibility, current selection, display
ordering, errors and final caller holder are unchanged.
