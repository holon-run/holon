---
status: accepted
date: 2026-11-26
---

# RFC: Runtime Usage Ledger

## Summary

Holon records provider usage in an attempt-level ledger stored in
`<holon-home>/.holon/state/runtime.sqlite` as the `usage_attempts` table. The
ledger is runtime state, not diagnostics: it is written through the runtime
database, migrated and backed up with it, and retained by the runtime retention
pass instead of the trace retention policy.

The ledger answers "what did this agent spend over a day, week, or year?" The
trace store keeps answering "what happened in this turn?" Sampling, span caps,
and the short trace retention window therefore cannot distort or delete usage
history, and a usage query never scans `trace_json`.

## Decision

### Counting unit

One row is one provider attempt that actually reached a transport. Fallback
rounds, retries, and interjection reselections each add one row per attempt,
and the winning response is the last attempt of the same dispatch. Rounds and
turns never add a second usage row, which removes the ambiguity where summing
`holon.provider.round` token totals and `holon.provider.attempt` token totals
double counts a fallback turn.

### Missing usage stays observable

An attempt can fail before the provider reports usage. Such a row stores `NULL`
in every token column with `token_usage_source = 'missing'`; it never stores
zeros. Aggregate reads report `missing_attempts` next to the summed tokens, so
an underestimated total is visible instead of silently presented as complete.
When usage exists it is the transport-normalized `attempt_token_usage.v1`
shape, not a second normalization owned by the ledger.

### Dimensions

Dimensions come from the resolved route: `provider`, `endpoint`, `model_id`,
and the full `model_ref` (`provider@endpoint/model`). `endpoint` is a separate
column because one provider id can fold several endpoints
(`opencode-go@default` and `opencode-go@messages`), and only the endpoint
segment identifies the billing party.

`transport` and `service_tier` are nullable. A failed attempt has no transport
report and no served tier; those stay `NULL` rather than being folded into an
`unknown` string that would lose information.

`day` is derived from `started_at` in UTC at write time so that by-day queries
never apply a function to the timestamp column.

### Attempt attribution

Each row carries `attempt_id` (primary key), `agent_id`, optional `turn_id`,
`work_item_id`, `task_id`, `round_index`, `attempt_index`, `started_at`,
outcome (`success`, `failed_retryable`, `failed_terminal`, `cancelled`), and an
optional `failure_category`. `trace_id` / `span_id` are soft, optional links:
a missing or pruned trace never invalidates a usage row.

`attempt_id` is derived from the attempt's own dispatch facts (turn, the
attempt's ordinal within its dispatch, route, and start time) instead of a
random value, so the primary key keeps its promise: re-reporting one attempt
through two paths stays an ignored duplicate. A round records its attempts once,
after its terminal result; when an abort keeps the carried attempts out of the
merged terminal timeline, they are recorded on their own so no dispatched
attempt loses its row.

## Retention

Usage rows follow `runtime.retention.enabled` like the rest of the runtime
database. Two keys were added and nothing else changed in the mechanism:

```toml
[runtime.retention]
usage_attempts_days = 400
usage_attempts_min_rows = 20000
```

The 400-day default covers year-over-year comparison; the 20000 row floor
mirrors `transcript_entries_min_rows`. `RuntimeDbRetentionReport` gains a
`usage_attempts` section so an operator can observe observed, candidate,
protected, planned, and deleted rows per pass. A pass deletes in bounded
batches and never goes below the floor. The ledger participates in the same
optional incremental vacuum as the other tables.

Because the ledger is append-only usage state with no strong roots, it has no
lifecycle protections beyond the row floor. Turn, WorkItem, or agent deletion
does not rewrite history: `turn_id`, `work_item_id`, and `task_id` are soft
references.

## Read surface

`RuntimeDb::append_usage_attempts` writes rows; `RuntimeDb::usage_daily_totals`
returns per-day totals with `attempts`, `reported_attempts`,
`missing_attempts`, and per-field token sums restricted to reported rows.
`RuntimeDb::usage_attempt_count` reports row counts. Presentation, pricing, and
cross-instance aggregation are out of scope for this contract.

## Verification

- attempts are counted once: an injected mix of one success, two fallback
  attempts with one missing usage, and one transport failure yields exactly the
  attempt count in rows, `SUM(uncached_input_tokens)` includes only reported
  rows, and `missing` rows equal one;
- `opencode-go@default` and `opencode-go@messages` land as separate endpoints;
- retention with `enabled = false` deletes nothing; with `enabled = true` and an
  exceeded age window it deletes in batches and respects the row floor;
- changing trace `sample_rate_permyriad` changes no usage row count;
- existing `state/runtime.sqlite` backup and restore cover the new table without
  a separate export path.

## Non-goals

- recording currency or prices, billing reconciliation, or invoicing;
- cross-instance, multi-tenant, or quota views;
- changing trace sampling, span caps, or trace retention;
- changing the UI `AgentTokenUsageSummary` display contract.

## Related

- [`runtime-db-retention`](runtime-db-retention.md) — the shared retention pass
  and its configuration surface;
- [`end-to-end-tracing-and-performance-observability`](end-to-end-tracing-and-performance-observability.md)
  — the trace plane this ledger is explicitly decoupled from.
