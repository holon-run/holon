# holon-test-fix-pr

## Purpose

Validate the public `holon solve` reusable workflow can fix a live
`holon-run/holon-test` pull request by pushing a follow-up commit.

This preserves the old manual `solve-holon-test-fix-pr` coverage, adapted to
the Rust workflow shape where the reusable workflow checks out the repository
and `holon solve` receives an explicit goal.

## Preconditions

- This repository's follow-up PR restoring the OIDC broker token path has merged
  to `holon-run/holon@main`.
- `holon-run/holon-test` has a workflow that calls
  `holon-run/holon/.github/workflows/holon-solve.yml@main`.
- The workflow has model-provider secrets and write permissions for contents
  and pull requests.
- A fixture PR exists in `holon-run/holon-test`, or the helper script can create
  one.

## Steps

1. Create a fixture PR with a markdown file that contains a concrete requested
   problem.
2. Record the PR head SHA.
3. Trigger the `holon-test` solve workflow with:
   - `ref`: the PR ref or URL
   - `goal`: fix the PR by editing the fixture file and pushing a follow-up
     commit
4. Wait for the workflow run to complete.
5. Download the `holon-solve-output` artifact.
6. Verify the PR head SHA changed.

## Expected

- The workflow completes successfully.
- `manifest.json`, `run.json`, and `summary.md` exist in the artifact.
- The fixture PR receives at least one new commit.
- The workflow log shows either a successful Holonbot broker exchange or the
  expected fallback path.

## Pass / Fail Criteria

- Pass:
  - Workflow exits 0.
  - Artifact exists.
  - PR head SHA after the run differs from the head SHA before the run.
- Fail (`infra-fail`):
  - Auth, token broker, checkout, provider, or workflow execution fails.
- Fail (`agent-fail`):
  - Runtime completes but no follow-up commit is pushed.

## Evidence to Capture

- PR URL.
- Workflow run URL.
- Head SHA before and after.
- `holon-solve-output` artifact.
- Relevant token-resolution log lines.
