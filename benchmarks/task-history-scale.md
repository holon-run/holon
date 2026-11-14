# Task history scale: public repository baseline

**Partial coverage:** this executable measures real public storage/repository
business reads. It does **not** cover runtime output-head preparation or
`RuntimeHandle::task_output` selected-output reads. In particular, it does not
establish a regression guard for #4105's historical Task-output scan. Both gaps
are explicit in every JSON artifact (`coverage_status: "partial"` and
`coverage_gaps`), not converted into passing results.

There is no standalone public output-head API. Native Task/Input transition and
ACK fixture APIs are crate-private. `AppStorage::append_task` and the public
Message bulk writer create canonical records, but do not qualify those records
for a Fresh runtime namespace. Constructing a `RuntimeHandle` afterward cannot
turn those writes into valid runtime Task-output fixtures. This benchmark does
not weaken that boundary, fabricate source indexes with SQL, or add production
exports. Full output-head/selected-output coverage requires an approved native
fixture entry point or an internal benchmark target with existing fixture access.

## Invocation

This is a manual, offline benchmark, not a new PR CI gate. Each process handles
one history size; the enclosing manual runner owns the scale matrix (suggested:
`0`, `2000`, `20000`, `100000`) and timeouts.

```sh
cargo bench --bench task_history_scale --no-run
HOLON_BENCH_SAMPLES=3 HOLON_BENCH_HISTORY_ROWS=2000 \
  cargo bench --bench task_history_scale
```

Invoke the compiled executable directly for machine ingestion so Cargo's own
output does not mix with JSON stdout. It writes exactly one JSON object after
all measured results validate. API, validation, and configuration errors exit
nonzero without a passing artifact.

- `HOLON_BENCH_SAMPLES`: positive integer, default `3`.
- `HOLON_BENCH_HISTORY_ROWS`: unsigned integer in `0..=100000`, default `2000`.

## Fixture and measured work

Every scale has four Task records, exactly two queued/active Tasks, and two
fixed Messages: an old Task result and an old supervised-invocation source.
`history_rows=0` omits only the extra history, retaining all fixed records.
Extra history consists of Task-result Messages for the same unrelated Task.
The exact selected result/source precedes all growing history; the result
cardinality of every measured operation stays one. Task-record count does not
grow, so its point lookup is a control, not a Task-table-size benchmark.

IDs, timestamps, insertion order, bodies, statuses, and cardinalities are
fixed. Database-assigned sequence numbers establish Message order. Setup uses
`AppStorage::append_task`, `AppStorage::append_message`, and
`RuntimeDb::messages().upsert_many_committed` in bounded batches. The invocation
source follows the minimal shape in the supervised-invocation repository tests;
this is not a claim to exercise the full supervised child-task lifecycle.
Fixture cardinalities are checked before measurement. Each process uses a new
temporary on-disk database, removed with the fixture.

| Result ID | Public business entry point | Verified result |
| --- | --- | --- |
| `task.history.selected_result_by_id` | `AppStorage::read_message_by_id` | Exact old Task-result Message ID, kind, Task ID, and body |
| `task.history.reverse_source_lookup` | `RuntimeDb::messages().supervised_invocation_sources` | Exact unique original invocation-source Message and body |
| `task.history.record_by_id` | `AppStorage::latest_task_record` | Exact historical Task ID and completed status |

The first operation measures selected **evidence** retrieval, not the runtime's
TaskOutput authorization, head selection, readiness, or output rendering. The
reverse lookup is an actual production API with its own historical scaling
behavior, not a replacement SQL implementation of Task output.

Each operation gets one untimed, validated warmup, then the requested number of
samples. Fixture setup, attribution snapshots, validation, environment discovery,
and serialization are outside the timed business operation. Bodies are decoded
inside the production API and retained until validation after the timer stops.
This is a warmed SQLite/OS-cache baseline, not a cold-disk benchmark. No runtime,
provider, daemon scheduler, command Task, or network request is launched.

Existing process-local stage counters are attached to real business samples;
reading counters alone is never timed as the benchmark. Stage `rows` has its
existing stage-specific meaning, not a SQLite scan/VM-step count. Stage times
can overlap and must not be summed. No query-plan, allocation, CPU-time, RSS,
or retained-memory coverage is implied.

## JSON contract

- `schema: "holon.task-history-scale.v1"`,
  `benchmark_suite: "task_history_scale"`, `workload_version: "v1"`,
  `started_at` (RFC 3339), `coverage_status: "partial"`.
- `coverage_gaps[]`: `benchmark_id`, `reason`; always includes
  `task.output.head` and `task.output.selected`. These are unmeasured coverage
  gaps, not successful or failed sample results.
- `environment`: `git_revision`, `git_dirty`, `build_source_sha256` (optional
  compile-time fingerprint, otherwise null), `os`, `arch`, `logical_cpus`,
  `rustc_version`, `debug_assertions`, `build_profile`, `network_mode`,
  `sqlite_version`.
  Unavailable command-derived metadata is null; network non-use is not an
  assertion of sandbox enforcement.
- `fixture`: `history_rows`, `fixed_message_rows` (`2`), `total_message_rows`,
  `task_rows` (`4`), `active_tasks` (`2`), `selected_records_per_read` (`1`),
  `warmup_repetitions` (`1`), `measured_repetitions`,
  `fixture_setup_wall_time_ns`, `cache_mode`.
- `results[]`: `id`, `operations_per_sample` (`1`), `samples[]`,
  `summary`.
- Samples: zero-based `iteration`, `wall_time_ns`, `exit_status: "success"`,
  `stages[]`. Each nonzero stage delta has `name`, `calls`, `rows`, `total_ns`.
- Summary: `median_wall_time_ns`, `min_wall_time_ns`, `max_wall_time_ns`,
  `median_absolute_deviation_ns`; even sample counts use the upper median.

Inspect raw samples and multi-scale trends. This baseline defines no automatic
latency threshold or overall coverage-pass verdict. Compilation and actual
multi-scale execution are required before interpreting any timings.
