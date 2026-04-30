# holon-test-review-pr

## Purpose

Validate the public `holon solve` reusable workflow can review a live
`holon-run/holon-test` pull request and publish review feedback without making
code changes.

## Preconditions

- This repository's follow-up PR restoring the OIDC broker token path has merged
  to `holon-run/holon@main`.
- `holon-run/holon-test` has a workflow that calls
  `holon-run/holon/.github/workflows/holon-solve.yml@main`.
- The workflow has model-provider secrets and write permissions for pull
  requests.
- A fixture PR exists in `holon-run/holon-test`, or the helper script can create
  one.

## Steps

1. Create a fixture PR with a small docs-only change in `holon-run/holon-test`.
2. Record the PR review/comment count.
3. Trigger the `holon-test` solve workflow with:
   - `ref`: the PR ref or URL
   - `goal`: review only; do not edit, push, or open another PR
4. Wait for the workflow run to complete.
5. Download the `holon-solve-output` artifact.
6. Verify a new PR review or PR comment was published.

## Expected

- The workflow completes successfully.
- `manifest.json`, `run.json`, and `summary.md` exist in the artifact.
- The PR receives review feedback or a PR comment.
- No new commits are pushed by the review-only run.

## Pass / Fail Criteria

- Pass:
  - Workflow exits 0.
  - Artifact exists.
  - Review/comment count after the run is greater than before.
- Fail (`infra-fail`):
  - Auth, token broker, checkout, provider, or workflow execution fails.
- Fail (`agent-fail`):
  - Runtime completes but no review/comment is published.

## Evidence to Capture

- PR URL.
- Workflow run URL.
- Review/comment count before and after.
- `holon-solve-output` artifact.
- Relevant token-resolution log lines.
