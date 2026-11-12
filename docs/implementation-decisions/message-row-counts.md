# Maintain exact Message counts when rows change

Runtime State publishes the exact canonical Message row count. A single SQLite
migration backfills counts by Agent. The same transaction that inserts, deletes
or moves a Message also updates its count. An upsert of an existing Message in
the same Agent and a receipt replay preserve the count. The final deletion
removes the empty counter. The runtime's existing State publication still uses
the count from the original transaction.

This replaces per-Agent history scans on the writer path with a primary-key
lookup. The migration scans existing Messages once. Global audit and diagnostic
queries can still count canonical rows directly.
