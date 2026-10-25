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

The companion [task-result recovery W2 gate](task-result-recovery-acceptance.md)
reuses this suite's isolation/build/cleanup mechanisms with real owned tasks,
strict durable-consumption oracles and separate fast/real-policy slow gates.
It does not change N02's review-only status or expand W1's verdict scope.

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
