# Hold the latest-Brief index behind its write-cost gate

Date: 2026-10-24. Baseline: `cd6ece8ee40c14290e7995e785dd91907892ca47`.
This records a local synthetic measurement, not a production rollout.

## Choice

Ship read-side attribution and a repeatable candidate-index benchmark without
adding a schema migration. Keep the nonunique
`briefs(agent_id, created_at DESC, evidence_id DESC)` index test-only until its
write-cost tradeoff is accepted or a separately approved workload resolves it.
Do not change latest ordering, remove other indexes, expand the reader pool,
or silently relax the initial write-p95 regression limit of 10%.

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
unindexed p95s by 18.3%, 25.6%, and 18.4%. The small absolute increase does not
by itself authorize changing the agreed gate.

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
Fresh/upgrade/reopen migration compatibility remains unverified because no
formal migration is being introduced.
