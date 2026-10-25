# Isolated task-result recovery acceptance

This is the W2 companion to [fallback-storm W1](fallback-storm-acceptance.md),
not a replacement or a promotion of W1's review-only N02 case. The runner uses
the same pinned-image/binary-revision checks, three internal networks, bounded
containers, read-only fixture mount, test-only named volume, and cleanup path.
It does not use production credentials, host ports, a real provider, or runtime
database writes. Runtime verification remains **pending** until a candidate
daemon actually passes these commands; Python tests verify the test assets only.

## Shared strict oracle

`scripts/docker_e2e/task_result_oracle.py` is also used by the existing live
command re-entry check and scheduler task/wait drill. It joins the original
task, result identity/message/generation/parent, captured owner, exact waits,
settlement, executed canonical activation, terminal outcome, and Result brief.
`settled/model_delivered` requires a nonempty settlement timestamp. The terminal
brief must be exactly `kind=result` with matching agent, owner, turn and related
message; a completed WorkItem must point to that exact brief. A Failure brief
containing the expected marker is not success.

The live command drill's terminal tool evidence accepts only a successful canonical TaskStatus envelope
for the exact terminal task, or a WaitFor `continue_turn` envelope bound to the
exact result and its current activation/already-delivered settlement. Debug
strings and arbitrary nested lookalikes are not tool evidence.

Failed attempts and resolved successor waiters are permitted; duplicate
successful consumption, stale identity, missing activation, or owner migration
fail closed. Scanner messages are checked through their durable source refs,
not by text. The snapshot includes `task_result_recovery_admissions`; request
counts, fallback count, queue processing, and resolved waits cannot substitute
for charged distinct roots or durable model consumption.

## Fixture and required gates

The provider script creates one real ready WorkItem, starts a promoted
ExecCommand behind a file gate in its isolated volume, and commits an exact
WaitFor(task_result). Only after the controller snapshots that owned waiter does
it release the command. Each subsequent provider request is held at a uniquely
identified provider barrier. The controller reads the real admission ledger
before releasing failure or success. It never edits deadlines, generations,
settlements, or scheduler state. A barrier timeout is a failed fixture.

| Suite | Cases | Required evidence |
| --- | --- | --- |
| fast | `exact-success`, `fallback-success` | Actual TaskResult root (including its legitimate fallback/replay successor), zero recovery charges, exact Result completion; fallback must actually exercise backup |
| fast | `alive-owner` | One open canonical attempt and nonterminal captured-owner turn remain unchanged beyond the persisted recheck deadline; no recovery charge/wake, followed by exact Result completion |
| slow | `fifth-success` | Four failed charged roots, fifth consumed successfully; primary RST and actual backup |
| slow | `exhausted` | Five distinct failed roots, one Failure diagnostic, pending unconsumed obligation, no automatic deadline, at least 65 seconds without a sixth admission |
| slow | `restart-backoff` | SIGKILL in charged backoff, same image/volume, unchanged identity/epoch/count/ledger/deadline, then fifth-root success |
| slow | `restart-fifth` | SIGKILL behind the fifth admission's provider barrier, no sixth charge on replay, then fifth-root success |

Production backoff is **60/120/240/480 seconds**: at least 900 seconds between
the first and fifth charged roots, plus setup, model and restart time. Each slow
case has a CLI minimum of **1800 seconds**; `--timeout 120` is rejected before
Docker is invoked. Release E2E runs four required matrix jobs, each with a
40-minute workflow timeout. The core/attestation job depends on all four.
PR scheduler-required runs all three fast cases as hard-failure steps; this does not
claim that a PR ran the slow suite. Python asset validation runs in
`make docker-e2e-validate` without Docker.

## Run a prebuilt candidate

Prepare the candidate and the existing Python fixture image separately, as
described in W1. The binary must embed a clean matching source revision; no
label-only fallback is accepted. The runner neither builds nor pulls images.
The read-only fixture directories and Python files must be readable by container
UID 10001 (normal checkout permissions are directories 755 and files 644).

```bash
python3 scripts/docker-task-result-recovery.py \
  --image holon:qa-candidate \
  --python-image holon-openai-responses-stub:local \
  --expected-revision "$(git rev-parse HEAD)" \
  --suite fast --timeout 180 \
  --evidence-dir target/docker-e2e/task-result-fast

python3 scripts/docker-task-result-recovery.py \
  --image holon:qa-candidate \
  --python-image holon-openai-responses-stub:local \
  --expected-revision "$(git rev-parse HEAD)" \
  --suite slow --case restart-fifth --timeout 1800 \
  --evidence-dir target/docker-e2e/task-result-restart-fifth
```

Omit `--case` to execute all four slow cases serially: budget up to two hours.
Fast cases each allow three minutes. These are timeout budgets, not measured
performance or a verified SLA. `make docker-e2e-task-result-fast` and
`make docker-e2e-task-result-slow` use the same commands with `DOCKER_IMAGE`.

Per-case evidence includes pre-terminal/captured identity, atomic read-only DB
snapshots, provider request-to-activation/root joins, policy, restart before/after
snapshots and mounted-volume identity, pinned build/isolation evidence, logs,
and cleanup status. Final evaluation uses the frozen database collected after
dispatch stops. Truncated/missing tables and cleanup errors cannot pass.
When a case is delivered through a scanner wake, the oracle requires that wake
message to be the activation's model input and joins it to the frozen task
result through its typed source refs; the raw task result stays a promise
source and is never expected in model input. A different or missing wake input,
a foreign result identity, and a second completed consuming turn all fail.
Exit 0 requires every selected case's frozen oracle to pass; interruption exits
130 after cleanup. A killed controller process cannot guarantee cleanup.

## Explicit remaining acceptance gaps

The runnable cases do **not** complete the full #3749 acceptance matrix:

- A provider barrier is reached after canonical admission. It cannot select
  the first exact-continuation versus `emit_due_task_result_recovery` scanner
  admission. A narrowly scoped daemon test barrier still requires review;
  no production scanner switch or database injection is introduced here.
- Observing five charges followed by persistent pause proves no sixth
  admission, not an explicitly attempted-and-rejected sixth root. The daemon
  drills therefore add no coverage for an explicit sixth wake.
- Coexisting-obligation fairness is not covered by these single-owner drills.

The first three gaps have deterministic in-process coverage that runs a real
command task against the real runtime, in
`src/runtime/task_result_recovery/live_command_tests.rs`: both scanner/exact
registration orders converge on one canonical admission, an explicit sixth
source fails closed with no charge and no provider call, and an exhausted
obligation does not starve a coexisting fresh result. In-process coverage is not
a daemon-side proof, so the manifest entries stay pending.

Scanner-first exact promises also survive provider failure and interrupted
claim replay. The startup regression runs the actual daemon startup transaction
before reconstructing the runtime, in both registration orders; invalid result
identity, conflicting wait ref and ordinary-followup origin remain quarantined.
These are real-command in-process tests, not Docker race-order evidence.
The same first-admission, provider-failure successor, bootstrap and daemon-startup
tests also run without a WorkItem, with lifecycle replacement, stop, cancellation,
expiry and forged-source fences. Replay asserts the persisted interrupted
predecessor, unchanged source identity/generation and binding, not an optional
turn-attachment marker.

`alive-owner` holds an actual provider request across the stored 30-second
recheck deadline, captures its open attempt and nonterminal owner turn before
and after that deadline, then releases success. It does not force a particular
scanner/exact race order. Its longer fixture/proxy and request timeout budgets
do not change runtime recovery deadlines or backoff. HTTP 400 is still eligible
for immediate fallback and never stands in for owner exit; failure cases release
failure on every resulting provider barrier and count durable recovery roots,
not HTTP failures.

These gaps are also recorded in `tests/e2e/docker/task-result-recovery.json`.
Passing this subset must not be reported as the full recovery matrix passing.
