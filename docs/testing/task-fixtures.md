# Explicit Task-ready test fixtures

State-only fixtures must remain State-only. Tests that need a current Task
domain can explicitly call
`runtime_db::test_support::prepare_current_task_fixture_tx` inside their existing
`RuntimeDb::transaction`. The helper is compiled only for tests in lib group `e`.

## Contract

The caller supplies the transaction, a diagnostic scenario name, the exact
`InputEvidenceScope` (including incarnation), a `TaskDomainLoadRequest`, and its
`InputEvidenceBudgetLedger`. The request retains the caller's query, capture,
recovery time, record limit, and byte limit; there is no shared default request.
The helper calls `prepare_task_domain_snapshot_with_budget_tx` and
`require_current`, returning the real `TaskCurrentSnapshot`. Errors retain their
original cause and add the scenario, agent, and incarnation.

This is an explicit preparation opt-in, not a fixture constructor or a write
qualification API. Keep Task seeds, write proofs, register-writes calls, and
assertions in the original test, in their original transaction and order.
Preparation does not seed Tasks, manufacture qualification, retry, or enlarge
the request or ledger budget.

## Pilot migration

| Module under `runtime_db::control_transition::tests` | Preparation sites | Preserved request |
| --- | --- | --- |
| `task_write_tests` | `seed_task` before/after the write; interrupt result before/after; ACK stop capture (5) | 4096 records, 8 MiB, current capture, no recovery time |
| `status_metadata_actor_tests` | `original_with_artifact` before the write (1) | 65536 records, 8 MiB, current capture, no recovery time |

The qualified/unqualified branch in `seed_task` is unchanged. A raw SQL Task
write is not made qualified by this helper. Tests deliberately exercising
missing Task domains, cold/legacy imports, retired scopes, recovery, or
insufficient qualification must not acquire blanket preparation. The helper
regressions use a real temporary database and transactions to check explicit
preparation/re-preparation, budget rejection, unqualified capture rejection,
and retired/no-State scopes without extra Task/qualification rows.

## Verification

Start with `make test-task-fixtures`. It enumerates lib group `e`, rejects empty
or duplicate selections, and executes these three modules with two test threads:

- `runtime_db::test_support::task_fixture_tests`
- `runtime_db::control_transition::tests::task_write_tests`
- `runtime_db::control_transition::tests::status_metadata_actor_tests`

Each group reports its test count and actual Cargo command. Enumeration failures
and any failed group fail the entrypoint. This does not replace the final gates:

```sh
python3 -m unittest tests.test_ci_timing tests.test_ci_test_shards tests.test_build_script
python3 scripts/ci_test_shards.py validate
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo check --all-targets
make test-shard SHARD=lib-a
```

Use a separate Cargo target for parallel worktrees. The full `lib-a` shard runs
compile groups `a`, `c`, and `e`.

## Remaining scope

Only the two pilot modules above are migrated. The #4074 `prepared_input_tests`,
`prepared_task_*`, production Task preparation/recovery code, and other fixtures
are not edited or copied. The approximately 50 historical fixtures in the issue
are an estimate, not this migration's count or an independently measured
performance baseline. Further migrations require separate scope/de-duplication;
no fixed speedup or complete #4074 delivery is claimed here.
