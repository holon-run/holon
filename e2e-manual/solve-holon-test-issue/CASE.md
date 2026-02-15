# solve-holon-test-issue

## Purpose

Validate `holon solve issue` end-to-end against `holon-run/holon-test` with a local-built agent bundle.

This case intentionally runs from a directory that is **not** the target repo workspace, so solve should use default workspace-prepare behavior (clone/prepare under resolved agent workspace root).

## Preconditions

- `./bin/holon` is built (`make build`)
- `gh auth status` succeeds
- `ANTHROPIC_AUTH_TOKEN` and `ANTHROPIC_BASE_URL` are available (env or `~/.claude/settings.json`)
- You have write permission to `holon-run/holon-test` issues/PRs

## Steps

1. Create a new issue in `holon-run/holon-test`.
2. Run `holon solve issue <issue-url>` with:
   - local bundle (`--agent`)
   - explicit `--agent-home`
   - explicit `--output`
   - `--cleanup none`
3. Check `manifest.json` for `status=completed` and `outcome=success`.
4. Verify PR reference can be extracted from output artifacts (`manifest.metadata` preferred; fallback to `summary.md`).

## Expected

- Solve run exits successfully.
- A PR is created for the issue.
- Artifacts are available under the test output directory.

## Pass / Fail Criteria

- Pass:
  - `holon solve` exits 0
  - output `manifest.json` exists and reports success
  - output artifacts contain created PR number/url
- Fail (`infra-fail`):
  - auth/runtime startup/publish pipeline failures
- Fail (`agent-fail`):
  - run completes but does not create/record PR metadata

## Evidence to Capture

- solve log
- issue/pr URLs
- output files (`manifest.json`, `summary.md`, optional publish artifacts)
- agent home file list

## Known Failure Modes

- Missing/expired Anthropic token.
- GitHub permission issues on `holon-run/holon-test`.
