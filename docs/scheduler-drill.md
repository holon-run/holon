# Scheduler drill

`scripts/scheduler-drill.py` is a host-side, resumable Docker runner for
collecting authoritative scheduler evidence from one fresh candidate node.
It never imports a report into the runtime and never writes scheduler protocol
tables directly.

The report is release acceptance evidence, not runtime scheduler authority.
Under
[Scheduler Cutover Simplification](rfcs/scheduler-cutover-simplification.md),
runtime manifest/preflight/scenario rows are retired from the engine-selection
contract.

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

## Docker Engine

The drill intentionally uses the native Docker Engine socket:

```bash
export DOCKER_HOST=unix:///var/run/docker.sock
```

`prepare` records the server version, storage driver, operating system, and
Docker root directory. Later commands refuse to continue if that identity
changes. The default requires `/var/lib/docker`; set `HOLON_DRILL_DOCKER_HOST`
only when selecting another native Engine socket with the same data-root
contract. Docker Desktop is not supported for this stress drill.

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

Start or restart the same authoritative candidate:

```bash
python3 scripts/scheduler-drill.py start --run-dir "$RUN_DIR"
python3 scripts/scheduler-drill.py exercise --run-dir "$RUN_DIR"
python3 scripts/scheduler-drill.py stop --run-dir "$RUN_DIR"
python3 scripts/scheduler-drill.py collect \
  --run-dir "$RUN_DIR" \
  --label authoritative-final
```

Use `kill` instead of `stop` at a crash checkpoint. `collect` requires the
candidate to be stopped and copies the named volume through a read-only mount.
It opens `runtime.sqlite` in SQLite read-only mode, removes the copied database
after collection, and leaves only `evidence.json`, `report.md`, and the secret
scan result.

Inspect resumable state with `status`; remove the
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
`stress-results.json`, and `stress-summary.json`. Operation evidence uses
event-sequence cursors and bounded state tails rather than repeatedly archiving
the full transcript and event history. Runtime SQLite/WAL copies are limited to
explicit semantic checkpoints and the stopped final collector.

The runner records Docker health plus host/container RSS, FD counts,
`runtime.sqlite`/WAL sizes, Docker stats, evidence size, and free disk at the
start, periodically, and at the end of a stress phase. Two consecutive Docker
control-plane failures open a shared circuit breaker; remaining operations are
marked aborted instead of continuing to pressure an unhealthy daemon.

Before a full matrix, create separate fresh runs at concurrency
`1 → 2 → 4 → 8`. Do not advance when Docker health fails or RSS, FD, DB/WAL,
evidence, or disk telemetry grows without a workload-explained bound.

## Evidence decision

The collector reports No-Go when any requested production scenario lacks a
completed stress operation, a current-revision hard blocker exists, JSON
evidence is malformed, or canonical activation/settlement/delivery tail state
is inconsistent. It also requires planned injections to have completed;
declared parameters alone never satisfy coverage. Historical hard blockers and
manifest/preflight revisions remain visible while the collector reads the
compatibility schema, but they do not grant runtime authority.

The historical rollout checks must be removed when those repositories are
retired. Release Go/No-Go is determined by collected scenario results and build
identity, not by importing or activating runtime rollout rows.
