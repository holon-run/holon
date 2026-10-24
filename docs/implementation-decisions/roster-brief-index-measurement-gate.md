# Latest-Brief index: accepted read/write tradeoff

Date: 2026-10-24. Baseline: `cd6ece8ee40c14290e7995e785dd91907892ca47`.
This records a local synthetic measurement, not a production rollout.

## Choice

Read-side attribution and a repeatable candidate-index benchmark shipped first,
without a schema migration, because the initial write-p95 regression limit of
10% was exceeded. On 2026-10-24 the operator explicitly accepted the measured
tradeoff below and authorized implementing the nonunique
`briefs(agent_id, created_at DESC, evidence_id DESC)` index.

Migration 85 adds only this index. Fresh databases use the existing release
baseline followed by the same migration; published historical migrations are
unchanged. Latest ordering, other indexes and reader capacity remain unchanged.
This acceptance is specific to this candidate and its disclosed workload, not
a general relaxation of performance gates.

## Evidence

The ignored test described in the [attribution RFC](../rfcs/read-side-performance-attribution.md)
uses bundled SQLite 3.45.0 and a fixed synthetic snapshot: 5 identities,
3 public active members including an empty history, 8,192 hot-Agent Briefs,
and 32 Briefs for each of the other 3 nonempty Agents. Each of 3 A/B/A rounds
restores the same snapshot per arm. A 40-sample run was followed by 200 samples
per arm; neither run changed the selected results.

In the 200-sample run, pooling raw samples across all rounds gives:

| Operation | Without index p95 | With index p95 |
| --- | --- | --- |
| Roster latest-Brief SQL | 3.81540 ms | 0.01064 ms |
| Single-Agent latest-Brief SQL | 3.77685 ms | 0.00501 ms |
| Full roster read | 4.02142 ms | 0.21032 ms |
| Serial single-Brief writer commit | 0.13067 ms | 0.15662 ms |

The roster statement's sort count falls from 3 to 0 and VM steps from 74,410
to 92. EQP selects the candidate covering index instead of a temporary sort.
However, each round's indexed write p95 exceeds the mean of its surrounding
unindexed p95s by 18.3%, 25.6%, and 18.4%. The small absolute increase did not
by itself authorize changing the agreed gate; implementation resumed
only after explicit acceptance.

Index builds took 16.898–19.308 ms on this fixture. Checkpointed database size
grew by 417,792 bytes; post-build WAL size was 436,752 bytes. These are not
peak temporary-disk measurements or a production migration budget.

## Preserved boundary

This fixture establishes a mechanism, not its share of live latency. SQL timing
excludes preparation and decodes the first column while draining all rows.
Full roster timing includes checkout, transactions and assembly; serial minimal
Brief inserts are not a production write mix. CPU, lease percentiles, concurrent
capacity, HTTP leaders and frontend-to-SQL amplification are not measured here.
No production records, DDL, deployment or restart are part of this decision.

## Upgrade behavior and recovery

`RuntimeDb::open_and_migrate` runs migrations synchronously before completing
database startup. Creating this index scans existing Briefs and writes the new
index; it does not rewrite message or transcript history. It can extend the
first upgraded startup and require additional database, WAL and temporary-disk
space. It uses the existing migration file lock and writer serialization, not
an online/background index builder.

The ordinary migration transaction commits index DDL and its migration marker
together. Failure rolls both back; a subsequent attempt can retry. Successful
reopens skip migration 85 rather than rebuilding the index. Tests compare a
real predecessor schema with a fresh database, preserve all synthetic Brief
contents and latest selections, and check schema-cookie and marker stability
on reopen. An injected marker failure exercises transactional DDL rollback.
Like existing evidence-index migrations, migration 85 checks whether `briefs`
exists: name-accepted legacy upgrade paths without that table record the
marker without recreating historical tables. Existing compatibility tests
and a missing-table/idempotence regression preserve this boundary.

The upgrade benchmark below measures the complete opening of the same
8,288-Brief synthetic predecessor snapshot three times, then successful
reopens. Standalone index-build timings above are not whole-startup timings.
Neither measurement predicts production downtime: live Brief cardinality,
payload/page layout, free disk, storage throughput and contention have not been
measured here. Deployment and any production DDL require separate authorization.

On 2026-10-24, the final bundled SQLite 3.45.0 debug-build benchmark measured complete
schema-84-to-85 opens at 39.63–41.21 ms and subsequent opens at 10.50–11.06 ms
across three runs. Each run preserved all 8,288 synthetic Briefs. Checkpointed
database size increased from 2,838,528 to 3,260,416 bytes (421,888 bytes), and
the post-upgrade WAL snapshot was 580,952 bytes. Unlike the standalone index
measurement, these figures include the normal startup and migration work.
They are not production estimates, tail percentiles or peak disk budgets.

```bash
cargo test --lib runtime_db::brief_latest_tests::brief_latest_migration_benchmark \
  -- --exact --ignored --nocapture
```

It emits one `brief_latest_migration_benchmark=` JSON line, including database
and WAL end snapshots, not peak temporary-space usage. There are no timing
assertions in the correctness gate.

Older binaries whose maximum supported schema is 84 reject a migrated schema
85 database. Merely reverting the executable or retaining/dropping this index
does not implement a supported downgrade. Plan a verified pre-upgrade backup
and recovery procedure for deployment; do not alter migration markers to force
an older binary to open the database.
