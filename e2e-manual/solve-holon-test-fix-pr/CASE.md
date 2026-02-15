# solve-holon-test-fix-pr

## Purpose

Validate `holon solve pr` in **fix mode** using `--skills github-pr-fix` against `holon-run/holon-test`.

This case runs solve with explicit `--workspace`, covering workspace-prepare path selection independent of shell CWD.

## Preconditions

- `./bin/holon` is built (`make build`)
- `gh auth status` succeeds
- `ANTHROPIC_AUTH_TOKEN` and `ANTHROPIC_BASE_URL` are available
- Push and PR permissions on `holon-run/holon-test`

## Steps

1. Clone `holon-run/holon-test` to a temp workspace.
2. Create a fixture PR with a simple markdown file.
3. Post a PR comment requesting concrete changes.
4. Record PR head SHA before solve.
5. Run `holon solve pr <pr-url> --skills github-pr-fix --workspace <local-repo>`.
6. Verify solve success and PR head SHA changed after run.

## Expected

- Solve run exits successfully.
- Agent pushes at least one follow-up commit to the PR branch.
- Artifacts are preserved for debugging.

## Pass / Fail Criteria

- Pass:
  - solve exits 0
  - manifest status/outcome indicates success
  - PR head SHA after run differs from before run
- Fail (`infra-fail`):
  - runtime/auth/publish failures
- Fail (`agent-fail`):
  - run claims success but branch head does not move

## Evidence to Capture

- solve log
- PR URL, head SHA before/after
- output artifacts (`manifest.json`, `summary.md`, `publish-results.json`)
- local workspace git status/log

## Known Failure Modes

- Agent handles comment but decides no code change is required (head SHA unchanged).
- Branch push denied due permissions.
