# Scheduler drill

`scripts/scheduler-drill.py` is a host-side, resumable Docker runner for
collecting scheduler shadow/cutover evidence from one fresh candidate node.
It never imports a report into the runtime and never writes scheduler protocol
tables directly.

## Credentials

Create an env file outside the repository and restrict it to the current user:

```text
VOLCENGINE_AGENT_API_KEY=...
DASHSCOPE_TOKEN_PLAN_API_KEY=...
```

```bash
chmod 600 /secure/path/scheduler-drill.env
export HOLON_DRILL_ENV_FILE=/secure/path/scheduler-drill.env
```

The path and values are not written to `run.json`. The control token is stored
separately under `target/scheduler-drill-secrets/` with mode `0600`. Set
`HOLON_DRILL_SECRET_ROOT` to keep it outside the checkout.

## Workflow

Validate both model routes with disposable containers:

```bash
python3 scripts/scheduler-drill.py preflight
```

Prepare a fresh candidate and record its run directory:

```bash
RUN_DIR=$(
  python3 scripts/scheduler-drill.py prepare \
    --iterations 1 \
    --concurrency 1
)
```

Start or restart the same volume in a requested scheduler mode:

```bash
python3 scripts/scheduler-drill.py start --run-dir "$RUN_DIR" --mode shadow
python3 scripts/scheduler-drill.py exercise --run-dir "$RUN_DIR"
python3 scripts/scheduler-drill.py stop --run-dir "$RUN_DIR"
python3 scripts/scheduler-drill.py collect \
  --run-dir "$RUN_DIR" \
  --label shadow-final

# After operator review:
python3 scripts/scheduler-drill.py start \
  --run-dir "$RUN_DIR" \
  --mode authoritative
```

Use `kill` instead of `stop` at a crash checkpoint. `collect` requires the
candidate to be stopped and copies the named volume through a read-only mount.
It opens `runtime.sqlite` in SQLite read-only mode, removes the copied database
after collection, and leaves only `evidence.json`, `report.md`, and the secret
scan result.

Use the same `start` command for the rollback chain:

```bash
python3 scripts/scheduler-drill.py start --run-dir "$RUN_DIR" --mode shadow
python3 scripts/scheduler-drill.py start --run-dir "$RUN_DIR" --mode legacy
python3 scripts/scheduler-drill.py start \
  --run-dir "$RUN_DIR" \
  --mode authoritative
```

Stop between mode changes. Inspect resumable state with `status`; remove the
container, network, volume, and private control token with `cleanup` only after
all reports have been retained.

`exercise` expands the persisted parameters into a deterministic stress plan:

- `iterations` runs every selected scenario that many times.
- `concurrency` creates that many dedicated `drill-agent-*` workers on the same
  candidate node; each worker runs its assigned operations in order.
- `duplicate-ratio` schedules duplicate trigger/rearm races.
- `stale-ratio` schedules stale trigger, out-of-order ingress, and wrong
  WorkItem-fence cases when the selected scenarios support them.

Every operation writes isolated evidence plus `stress-plan.json`,
`stress-results.json`, and `stress-summary.json`. The phase is marked failed
only after all workers finish, so one failure does not discard the remaining
evidence.

## Evidence decision

The collector reports No-Go when any production scenario lacks comparison
evidence, a comparison diverges, a current-revision hard blocker exists, JSON
evidence is malformed, or canonical activation/settlement/delivery tail state
is inconsistent. It also requires the recorded stress operations and planned
injections to have completed; declared parameters alone never satisfy coverage.
Historical hard blockers remain visible but do not count as current unless
their config/manifest/preflight fences match current authority.
