# Reuse Task metadata parsing within its original read

Task write proofs and preflight revision checks read the scoped revision column
without decoding coverage. The subsequent snapshot preflight still decodes the
coverage, verifies its embedded revision against that column, and runs before
business SQL. Finalization keeps its original complete metadata read.

The metadata node-count statement materializes one temporary JSONB value and
uses it for the existing selectors. This avoids repeated text parsing while
retaining the SQL/Rust count comparison, including duplicate-entry rejection.
Encoded-byte observation still precedes native JSON traversal. SQLite 3.45.0 is
already bundled; JSONB stays inside the statement and is never persisted.

This does not cache a result across reads, change the stored coverage format,
or alter TaskResult settlement, source qualification, owner/incarnation fences,
resource ownership, or receipt replay. Remaining snapshot materialization still
runs in its original transaction.
