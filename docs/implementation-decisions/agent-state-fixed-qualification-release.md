# Fixed qualification is part of one release

The Runtime State migrations v93–v104 ship together in this change. Intermediate
development commits are not independently released database formats.

| Version | Durable fact added | Why it is needed |
| --- | --- | --- |
| 93 | Episode-memory command results | Replay an original result without repeating finalization. |
| 94 | Episode continuity seeds, membership and imports | Freeze the legacy scope once; isolate recreated identities. |
| 95 | Start/Stop receipts and cancellation obligations | Keep command replay and unfinished cancellation explicit and durable. |
| 96 | Input/evidence qualification and source metadata | Distinguish accepted evidence from later raw history. |
| 97 | Original input-mutation receipts | Recover the original write result rather than infer success from current state. |
| 98 | Work/Wait/continuation captures and qualified records | Preserve exact Work results and Wait dependencies across resident-window trimming. |
| 99 | Dispatch-memory responsibility checkpoints | Record actual dispatch acknowledgements rather than infer responsibility from later history. |
| 100 | Task/result qualification and query/status source metadata | Preserve the original Task/result scope and its accepted source ordering. |
| 101 | Scheduling manifests, captures and qualified records | Make the scheduling snapshot's accepted membership explicit. |
| 102 | Input birth cursors, membership and lookup indexes | Admit native first-write facts without borrowing authority from a later query. |
| 103 | Current Queue/Event qualification and historical Event acknowledgements | Separate live Current membership from retained Event history. |
| 104 | Turnless command-task creation sources and settlement linkage | Give command creation a real source instead of fabricating a parent Turn. |

These are not twelve copies of Agent state. They record distinct original
acknowledgements, eligibility boundaries and replay results that a later SQL read
cannot reconstruct safely.

## One release does not imply one migration

`RuntimeDb::migrate` applies these ordered steps automatically. Each migration
commits its schema, any fixed data capture and its `schema_migrations` entry in
the same transaction. The entire chain is not one transaction: a failed step
rolls back that step, while earlier committed steps remain recorded for retry.
The capture steps depend on earlier continuity and qualification facts.

Keep the versioned domain steps rather than renumbering them merely to match
the PR boundary. Known development installations already have these ledger
entries, and `migration_history.rs` recognizes only exact supported lineages.
Collapsing them would require an explicit compatibility mapping and equivalent
capture-order, rollback and upgrade tests; squashing Git commits does not do
that. This is a release-packaging choice, not permission to replay a fixed cut.

The [legacy upgrade fixes](legacy-recovery-upgrade-evidence.md) correct capture
and validation at the original boundary without adding a schema version. They
do not retroactively fill an already sealed, incomplete development checkpoint;
that requires a separately approved, backup-proven repair.

The fixed upgrade lease captures existing legacy scopes at v94 and qualifies their bounded input/evidence metadata at v96. A newly created or recreated identity receives `fresh_empty` continuity and cannot adopt records left under the same agent name. Reopening the database never widens either fixed checkpoint to include later records.

The checkpoint freezes canonical body identity separately from observer revisions. Turn records have no canonical content hash, so the checkpoint includes their bounded body digest before any later first materialization. Intermediate development databases missing that digest are not adopted on read; releasing such a format separately would require an explicit migration policy as well.

If v94 is released separately in the future, upgrading that release requires an explicit input/evidence adoption actor and policy. Treating all v94 `fresh_empty` scopes as legacy would violate identity isolation. Historical reads remain separate from current-scope qualification.
