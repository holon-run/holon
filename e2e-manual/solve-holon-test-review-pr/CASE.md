# solve-holon-test-review-pr

## Purpose

Validate `holon solve pr` in **review mode** using `--skills github-review` against `holon-run/holon-test`.

This case runs solve from inside a local clone of the target repo to cover workspace-prepare behavior that reuses current repo context.

## Preconditions

- `./bin/holon` is built (`make build`)
- `gh auth status` succeeds
- `ANTHROPIC_AUTH_TOKEN` and `ANTHROPIC_BASE_URL` are available
- Push and PR permissions on `holon-run/holon-test`

## Steps

1. Clone `holon-run/holon-test` to a temp directory.
2. Create a small docs change branch and open a PR.
3. Record review count before solve.
4. Run `holon solve pr <pr-url> --skills github-review`.
5. Verify solve success and that PR review count increases.

## Expected

- Solve run exits successfully.
- At least one new review is published to the PR.
- Output artifacts are saved.

## Pass / Fail Criteria

- Pass:
  - solve exits 0
  - `manifest.json` reports `completed/success`
  - PR review count after run > before run
- Fail (`infra-fail`):
  - clone/auth/runtime/publish path failures
- Fail (`agent-fail`):
  - solve success reported but no new review published

## Evidence to Capture

- solve log
- PR URL and before/after review counts
- output artifacts and local repo branch head

## Known Failure Modes

- Missing git identity for fixture commit.
- API propagation lag when fetching review count immediately after publish.
