# Isolated fallback-storm acceptance

This opt-in fault suite runs the real `holon serve` process through its HTTP
contracts. It complements [Docker release acceptance](docker-acceptance.md);
it is not the real-provider release gate and does not declare release readiness.
The runner uses the existing `docker_e2e` evidence helpers and deterministic
OpenAI Responses fixture, with an additional TCP fault proxy.

## Implemented scope

| Case | Injected fault | Required observation |
| --- | --- | --- |
| C00 | Healthy primary and backup | One primary HTTP 200, one completed turn, one result brief |
| N01 | Primary TCP RST | Primary exercised; exactly one backup HTTP 200 and one delivery |
| N02 | Both routes TCP RST | Both exercised, no HTTP or successful delivery; durable budget evidence retained for review |
| N03-headers | Primary accepts TCP but sends no headers | Backup completes under the configured request deadline |
| N03-body | Primary sends headers and an unfinished JSON body | Backup completes under the configured body deadline |
| N03-idle | Primary opens SSE and then stalls | Backup completes under the configured stream-idle deadline |
| H01-429 | Primary HTTP 429 with `Retry-After: 1` | Healthy backup completes without rapid epoch churn |
| H01-503 | Primary HTTP 503 | Healthy backup completes within the dispatch bound |

These are W1 workloads: one prompt, no tools. Local deadlines are deliberately
shortened; this does not validate production-default deadlines. The providers
use `auth.kind=none`, so 429 evidence covers the **no quota identity** path,
not credentialed providers or account-wide throttling.

The [recovery contract](../rfcs/turn-model-lineage-and-recovery.md) defines a
three-dispatch persistent network epoch, including the initial dispatch.
The runner checks that budget against the database, not just HTTP counts.
The wire watchdog allows later N02 epochs at a conservative 10-second floor;
this is a safety ceiling, not proof that cooldown timing is correct. There is
no invented finite lifetime request cap for a permanent network outage.

## Isolation and prerequisites

- Docker and Python 3.10+ on the execution host.
- Prebuilt local Holon and Python fixture images. The runner never builds or
  pulls images. Prepare images separately, before starting fault injection.
- Each case creates three `--internal` networks, a new `HOLON_HOME` volume,
  and four bounded containers: daemon, controller, proxy, mock.
- No published host ports, host networking, Docker socket mount, production
  configuration, credentials, or real model endpoints.
- Only `tests/e2e/docker` is bind-mounted, read-only. The controller reads only
  the new case volume. A fresh random control token is passed solely to the
  daemon and controller request process; it is not written to evidence.
- The runner verifies mounts, network isolation, and absence of a default
  IPv4 route before creating the workload.

Do not substitute the shared daemon, a production `HOLON_HOME`, or a live
database. Image building or pulling may need network access; the fault run
itself must remain isolated. The local Docker engine is a trusted prerequisite,
not a strong sandbox against a malicious image.

## Run

Select a prebuilt candidate whose binary embeds a clean source revision, check
it without networking, and build the existing Python fixture image:

```bash
HOLON_QA_IMAGE=holon:qa-candidate
docker run --rm --pull=never --network none \
  --entrypoint holon "$HOLON_QA_IMAGE" --version
docker build -t holon-openai-responses-stub:local \
  tests/e2e/docker/openai_stub
```

The candidate binary must embed a source revision. An OCI revision label alone
does not establish build identity; unknown, dirty, or mismatched binary
revisions fail. The stock Docker build excludes `.git` from its context, while
`build.rs` obtains its revision from Git. Consequently, `make docker-build`
alone does not currently guarantee a revision-bearing binary. Use an
independently identified, revision-bearing candidate; resolving that packaging
gap is outside this test-only change. Do not work around the check by changing
a label or fabricating a binary version.
Provide the source revision of the **binary being tested**, not necessarily the
checkout containing the runner:

```bash
python3 scripts/docker-fallback-storm.py \
  --image holon:qa-candidate \
  --python-image holon-openai-responses-stub:local \
  --expected-revision "$(git rev-parse HEAD)" \
  --case C00 --case N01 --case H01-503 \
  --evidence-dir target/fallback-storm/smoke
```

Omit `--case` to run all eight cases, including known-red and review-only cases.
Tags are resolved to immutable image IDs once before any case starts. The
running binary's `holon --version` revision is checked before workload ingress.
The build check accepts the binary's abbreviated revision (at least eight hex
characters); it is not full-hash reproducible-build attestation.

Useful options:

- `--timeout 120`: maximum observation window per case, after bootstrap.
- `--quiet-seconds 20`: minimum stable window before evaluating. N02 requires
  at least 65 seconds of quiet and may time out while legitimate probes recur.
- `--http-timeout 3`: baseline request deadline in seconds. N03 cases override
  individual deadlines so header, body, and idle paths can be exercised.

A request-budget violation stops observation early, preserving the offending
snapshot. Later selected cases still run. Exit `0` means every selected case
is `verified-w1-isolated`; any failure, incomplete evidence, cleanup error,
or `needs-parking-review` yields exit `1`. Ctrl-C cleans up the current case,
writes its summary, and exits `130`. A killed host process cannot guarantee
cleanup.

## Evidence and verdicts

`summary.json` points to a uniquely named per-case directory containing:

- `build.json` and `readiness.json`: pinned image IDs, expected source revision,
  observed binary version, and real service readiness.
- `topology.json`, `networks.json`, `*-route.txt`: isolation checks.
- `policy.json`: workload, deadlines, and request watchdog bounds.
- `bootstrap-*.json`, `prompt.json`: healthy bootstrap baseline and root ingress.
- `events.json`, `state.json`, `mock-http.json`, `proxy-tcp.json`: bounded
  observation snapshots excluding bootstrap requests.
- `*.log`, `database.json`, `budget-analysis.json`: final container logs,
  read-only SQLite snapshot, and canonical-root/epoch/request join.
- `cleanup.json`: collection and cleanup errors; these prevent a green result.

Pass requires actual wire activity, one successful terminal with the exact
marker, and one result brief. A failure brief that quotes the marker is not
success. Empty or truncated evidence is never a pass.

The budget join requires one episode for the original prompt, unique durable
request IDs, at most three dispatches per epoch, matching durable totals/wire
counts, and no unexplained empty model turns. Dispatch is stopped before final
log/DB collection. Earlier HTTP/TCP/event polling snapshots are **not atomic**:
a count mismatch fails closed for investigation, not automatic attribution to
a product defect. Keep durable dispatch counts distinct from observed HTTP
responses when reporting a storm.

N02 deliberately cannot return a green verdict: review `state`, `not_before`,
parking, and successive epochs. A quiet interval is not proof of permanent
parking or restart-safe recovery. This suite does not yet enforce exact server
`Retry-After` timing across restart.

## Regression baseline and remaining gates

Run the existing CI validation entry point; it includes the new oracle and
runner lifecycle regressions and does not need Docker or provider credentials:

```bash
make docker-e2e-validate
```

The container suite stays opt-in; adding it as a PR/nightly/release blocking
job requires a separate decision. Do not hide a known-red case with an
expected-failure success exit.

The original 2026-10-25 run of binary `d841d82f` found:

- C00, N01, N03 variants, H01-503: scoped W1 passes.
- N02: observed persistent cooling; long-outage/restart closure not established.
- H01-429: rapid epoch churn without effective cooldown in the unauthenticated
  branch, tracked in holon-run/holon #3748. This is historical evidence, not a
  claim about every later candidate.

L02 exact `WaitFor` recovery and R01 scanner/budget-restart checks are **not
implemented by this runner**. Earlier separate probes exposed a failure-brief
false-positive and incomplete recovery evidence, tracked in #3749; their
failed/blocked status must not be promoted to passing by this W1 suite.
Remaining gates include durable single consumption/delivery, exact waiter and
owner preservation, restart near budget exhaustion, long outages, concurrent
tasks, absolute `Retry-After`, stream truncation/malformed responses, and real
provider integrations. Unit tests and mock results cannot satisfy those gates.

## Cleanup

The runner removes only the case's recorded containers, networks, and volume,
even if log collection fails. Resources carry
`holon.e2e.suite=fallback-storm`; inspect `cleanup.json` and inventory before
removing any residue after a forced termination. Never use an unscoped
`docker system prune`. Evidence remains in the requested output directory;
review/redact it before publishing, and do not commit raw runtime databases.

## Tool continuation and interjection cases (#3753 / #3754)

`scripts/docker_e2e/continuation_interjection.py` reuses the same isolated
topology, controller, and evidence helpers for two additional cases. It is
separate from the W1 fault cases and does not treat C00/N01 as evidence here.

| Case | Deterministic trigger | Required observation |
| --- | --- | --- |
| T3753 | A serves one tool call (`ExecCommand` gated on `/var/lib/holon/qa-3753-gate`); a second agent gets a fixture 503 from A, writing a durable transport cooling fence; the gate is released afterwards | A receives no continuation request carrying tool output; B receives exactly one; one `provider_candidate_reselected` with `dispatch_occurred=false` and `predicate=scope_cooling`; the gated tool executes once; one completed turn and one result brief |
| I3754 | The real `QA_PARK` operator input is admitted on healthy A and returns provider-owned encrypted reasoning plus a gated tool (`/var/lib/holon/qa-3754-park-gate`); helper qa2 seeds only A's transport cooldown with `QA_COOL_CHAIN` 503 (`Retry-After: 120`), leaving B healthy. After reading the canonical A fence the gate is released: the nonportable continuation parks locally without crossing lineage. The original A/B chain is unchanged. A 300 ms timer independently admits on B, with its response held before the tool call; a fresh prompt is queued before release | The exact operator prompt message ID settles `interrupted` with `provider_dispatch_parked` (never substituted by internal lineage recovery or interjected); opaque reasoning and independent B timer evidence are required; the fresh input becomes `interjected`; exactly one `operator_interjection_admitted`; no timer aborted/runtime-error terminal; the timer turn completes with one result brief |

T3753 exercises the local guard path: the cooling fence is read back from
`provider_transport_incidents` before the gate is released, so the fixture does
not rely on a sleep-only race. Incident refs are endpoint-qualified (for example
`qa-a@default/gpt-5.4`), and the tool-call log includes its marker in the same
record. I3754 exercises the interjection planner; the
parked source must be retained rather than consumed as an interjection, while a
later queued input must still make durable progress.

The provider-owned reasoning must include
`content: [{"type": "reasoning_text", "text": "QA_PROVIDER_OWNED_REASONING"}]`;
`encrypted_content` alone is ignored by the Responses parser. The runner also
requires the source turn's persisted `assistant_round` transcript `data.blocks`
to contain the parsed `reasoning_text` block. These blocks are the serialized
assistant blocks; terminal `turn_records` do not contain an `assistant_blocks`
field. `original-reasoning.json` preserves the source-linked evidence.

I3754 holds the timer's provider response **before returning its tool call**,
queues the fresh prompt, and captures `before-tool.json` while the old source
is `interrupted` and the fresh source is `queued`. The actual interjection
audit must name the fresh message and `before_tool_execution`; a later
provider-dispatch admission does not pass. `before-timer.json` and
`after-timer.json` preserve the original canonical message, input-consumption
owner, and parking deadline. Input consumption ownership is read from
`conversation_input_assignments`, linked to the original source turn;
`scheduler_activation_inputs` is retained for audit but no longer written, so it
is not an ownership oracle. The completed timer turn's `input_message_ids`
must include the fresh input and exclude the parked source. Source and timer
turns must not abort, and the timer must finish before the original deadline.
The runner then waits for the original deadline without modifying runtime
state: one scheduler reclaim, one `interrupted_queue_claim_reentry` replay
with the same turn owner, and exactly one `Processed` settlement are required.
Recovery events are fetched before stopping the daemon's HTTP server; final
database and provider-log evidence is captured after dispatch is stopped.
Only the complete pre-deadline plus recovery sequence returns
`verified-i3754-isolated`. These are canonical/audit observations; the runner
does not expose a direct heap snapshot of the runtime's in-memory queue.

Run only after the parent supplies a revision-bearing candidate built from the
accepted revision. Do not use the dirty QA worktree or a locally built binary
for the runner gate, and do not bypass the revision check.

```bash
python3 scripts/docker-continuation-interjection.py \
  --image holon:qa-candidate \
  --python-image holon-openai-responses-stub:local \
  --expected-revision "$CANDIDATE_REVISION" \
  --case T3753 --case I3754 \
  --evidence-dir target/continuation-interjection
```

Per-case evidence contains `build.json`, `readiness.json`, `events.json`,
`http.json`, `snapshot.json`, and `cleanup.json`; `summary.json` carries one
status per case (`verified-t3753-isolated` or `verified-i3754-isolated` only on
full pass). The snapshot includes queue, provider parking, incidents, tool
executions, and messages so queue/turn terminal state and the single tool
execution are reviewable. The runner does not read other agents' data or a live
database, publishes no ports, and uses only the internal Docker networks and
the task-local volume. Exit `1` means failed or blocked evidence, never a pass.
`--http-timeout` defaults to 45 seconds; `--timeout` defaults to 180 seconds
per bounded wait, including the original 120-second source cooldown.
