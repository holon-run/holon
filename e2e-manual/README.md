# E2E Manual

This directory stores manual end-to-end test cases for Holon.

Each subdirectory under `e2e-manual/` is one test case.

## Structure

- `CASE.md`: Human-readable test case definition and pass/fail criteria.
- `run.sh`: Optional script to execute the test flow.
- `collect.sh`: Optional script to collect logs and evidence.
- `artifacts/`: Optional local output directory for collected evidence (gitignored).

## Case List

- `serve-github-issue-solve`: Validate `holon serve` issue-comment trigger flow end-to-end.
- `serve-autonomous-project-drive`: Validate `holon serve` + `message send` for autonomous milestone decomposition and `@holonbot` execution trigger.
- `run-pptx-remote-skill`: Validate `holon run` auto-init + remote `pptx` skill + local agent bundle.
- `solve-holon-test-issue`: Validate `holon solve issue` against `holon-run/holon-test` (default workspace prepare path).
- `solve-holon-test-review-pr`: Validate `holon solve pr --skills github-review` from within a local `holon-test` clone.
- `solve-holon-test-fix-pr`: Validate `holon solve pr --skills github-pr-fix` with explicit `--workspace`.

## Add a New Case

1. Copy `e2e-manual/_template` to a new case directory.
2. Fill `CASE.md` with concrete repo, trigger, and expectations.
3. Implement or adjust `run.sh` and `collect.sh` if automation is useful.
4. Ensure generated outputs are written to `artifacts/`.

## Failure Classification

- `infra-fail`: Event or runtime pipeline is broken (webhook, channel, runtime startup).
- `agent-fail`: Pipeline works but agent behavior misses expected outcome.
