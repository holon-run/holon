# Agent relation write cutover and explicit backfill

New Agent creation and terminal lifecycle writes commit the compatibility
identity row and canonical relation/policy records in one SQLite transaction.
The legacy identity fields remain populated so older readers can continue to
read the database during Phase F, but no production create path may commit a
legacy-only identity.

Historical data is migrated only through
`holon debug runtime-db agent-relations`. The default mode is a read-only
report that does not upgrade the database schema; `--apply` requires the
offline maintenance lock and creates a verified pre-migration database backup
unless `--no-backup` is explicitly supplied. Backfill reuses the same per-axis
projection used by compatibility reads, migrates only legacy axes without
diagnostics, and records bounded unresolved evidence instead of guessing.

An apply run commits each Agent and its checkpoint in one transaction. A
restart reuses the durable `running` run and skips completed Agent checkpoints;
an explicit failed run is retained for audit and a retry starts a new run.
Backfill never rewrites Agent identity, AgentHome, history, tasks/results,
workspace ownership, deletion jobs, or tombstones.

Schema 61 is additive. Phase F rollback therefore means restoring a verified
pre-upgrade database backup before starting an older binary: older binaries
reject a newer schema version rather than silently ignoring canonical state.
Mixed-version compatibility is provided at the data-contract level by the
retained identity mirror, not by opening schema 61 with an older binary.
