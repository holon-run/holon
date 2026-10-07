# Test Duty Matrix

This document records the current test and validation topology. It is a
navigation aid, not a new test policy: the commands, target membership, and CI
conditions below are the existing contracts as of 2026-10-07.

## Invariants

- The default fast path must not require provider credentials, a live daemon,
  or a real-model network call.
- `#[ignore]` live tests remain opt-in; live acceptance must stay visibly
  separate from deterministic unit, protocol, and lifecycle coverage.
- Concurrent lifecycle tests are not silently duplicated in the serial suites.
- Every Rust test target remains assigned to a CI shard or the explicit
  concurrent suite; shard drift is checked by `scripts/ci_test_shards.py`.
- A command name describes its actual coverage. Local convenience commands are
  not presented as a complete substitute for platform, Docker, coverage, or
  live CI jobs.

## Current matrix

| Layer | Canonical entry | Responsibility and scope | Dependencies / cost | Failure diagnosis |
| --- | --- | --- | --- | --- |
| Package and SDK checks | `npm test` in a package; `make app-sdk`; `make web-ci` | TypeScript build plus package-local deterministic tests. `make app-sdk` builds the sibling API SDK before the App SDK. | Node.js 24 for current CI paths; package dependency installation. No provider credential or live daemon is part of the test contract. | Package test output and TypeScript diagnostics; use the package command directly for isolation. |
| Rust static and build gates | `make fmt-check`, `make lint`, `make build`, `cargo check --locked --features local-onnx`, `make snapshots-check` | Formatting, clippy, all-target compilation, feature-gated compilation, and generated snapshot drift. | Rust toolchain; no live provider. Build cost is higher than a focused test target but remains deterministic. | The failing command identifies the gate; snapshot commands identify the affected inventory. |
| Serial Rust integration targets | `make test` | Runs 48 Cargo integration-test targets with one test thread, excluding the six targets owned by the concurrent suite. It does **not** run library/binary unit tests despite the former help text. | Rust only; live targets are not selected with `--ignored` by this entry. Broad local runtime cost. | `scripts/ci_test_shards.py` prints the target command; rerun one target with `cargo test --test <name>`. |
| CI Rust test shards | `make test-shard SHARD=lib\|control\|cli\|misc` | Four parallel CI responsibilities: lib/bins, six HTTP control targets, ten CLI/snapshot targets, and the remaining 32 targets. The validated total is 54 Cargo test targets. | Rust; `lib` uses two test threads, other shard commands use one. `misc` contains live-named binaries but does not pass `--ignored`. | Each matrix job uploads parsed target timing from `scripts/ci_timing.py`; rerun the named shard locally. |
| Concurrent lifecycle tests | `make test-concurrent`; on main, `make test-concurrent-repeat CONCURRENT_REPEATS=2` | Six runtime/worktree lifecycle targets run with Rust's default test threading to exercise interleavings excluded from serial coverage. | Rust; intentionally concurrency-sensitive. Main pushes repeat the core set. | Run the named integration target alone, then repeat with the same target set. |
| Provider, image, and runtime live smoke | `make test-live`, `make test-live-openai`, `make test-live-anthropic`, `make test-live-codex`, `make test-live-xai`, `make test-live-images`, `make test-live-runtime` | Explicit provider transport, image, prompt-continuity, and workspace-tool acceptance probes. | Credentials, network access, and sometimes local auth state; all use `--ignored --nocapture`. Never part of the default fast path. | Preserve full `--nocapture` output and record provider/model configuration without publishing secrets. |
| Docker and scheduler boundaries | `make docker-e2e-validate`, `make docker-smoke`, `make docker-e2e-scheduler-required`, `make docker-e2e-scheduler-live-canary` | Manifest/runner unit validation, image readiness, deterministic scheduler E2E, and an optional real-model canary. | Docker. The live canary additionally needs protected provider configuration; required scheduler E2E is deterministic. | Inspect runner validation first, then the generated scheduler evidence/artifacts. |
| Platform and full-sweep validation | CI `web`, `swift-sdk`, `ios-app`, `android`, `macos-*`, `coverage`, and release jobs | Platform-specific SDK, simulator, app, packaging, coverage, release, and artifact contracts that cannot be represented by the Rust fast path. | Hosted platform toolchains, simulators, Docker, or nightly/manual full-sweep capacity. | Use the named CI job and uploaded artifact; do not substitute a local Rust command for a platform gate. |

## Gate boundaries

### Local deterministic subset

`make ci` is the local deterministic subset:

```text
web-ci conversation-sdk-ci fmt-check lint build snapshots-check
test-resource-lint test
```

It intentionally does not claim to replace concurrent, platform, Docker,
coverage, or provider-backed live jobs. `make check` is a smaller quick check
for formatting, clippy, and compilation.

### Pull request Rust path

The Rust CI path runs static/build checks, four `rust-test` matrix shards, and a
separate `rust-concurrent` job. Coverage is restricted to scheduled or manual
full sweeps; path filters decide which platform and Docker jobs are needed.
`scripts/ci_test_shards.py validate` is the drift guard for target membership.

### Live and release path

Provider-backed live commands and scheduler canaries are explicit acceptance
layers. They require credentials and network access and must not be folded into
the default deterministic gate merely because a test binary is already listed
in the `misc` shard.

## Evidence and maintenance rules

- When adding a Rust integration target, run `python3 scripts/ci_test_shards.py
  validate` and confirm it lands in the intended shard.
- When changing concurrency-sensitive lifecycle behavior, run both the focused
  target and `make test-concurrent`; do not move it into serial coverage
  without equivalent interleaving evidence.
- When changing a live probe, preserve its `#[ignore]` reason and update the
  corresponding `make test-live-*` entry if credentials, provider scope, or
  output expectations change.
- When changing a CI job or local Make target, update this matrix in the same
  change and run `git diff --check`.

This baseline does not move, merge, or delete tests. Any such change requires a
separate equivalence record covering target membership, dependencies, failure
diagnostics, and observed runtime cost.
