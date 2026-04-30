# holon-test-issue-resolve

## Purpose

Validate the public `holon solve` reusable workflow can resolve a live
`holon-run/holon-test` issue and publish a PR.

This case covers the GitHub Actions path, including token resolution:

1. explicit `holon_github_token` secret when provided
2. Holonbot token broker exchange through GitHub Actions OIDC
3. fallback to `github.token`

## Preconditions

- This repository's follow-up PR restoring the OIDC broker token path has merged
  to `holon-run/holon@main`.
- `holon-run/holon-test` has a workflow that calls
  `holon-run/holon/.github/workflows/holon-solve.yml@main`.
- `holon-run/holon-test` has model-provider secrets available.
- The workflow grants `contents: write`, `issues: write`,
  `pull-requests: write`, and `id-token: write`.
- The Holonbot token broker is expected to accept audience
  `holon-token-broker`.

## Steps

1. Create a fresh issue in `holon-run/holon-test` asking for a small, concrete
   docs-only change.
2. Trigger the `holon-test` solve workflow with:
   - `ref`: the issue ref or URL
   - `goal`: solve the issue, commit the docs change, push a branch, and open a
     PR
   - `build_from_source`: `true` if validating a just-merged workflow change
3. Wait for the workflow run to complete.
4. Download the `holon-solve-output` artifact.
5. Verify a PR exists for the issue.
6. Verify the workflow log shows either a successful Holonbot broker exchange or
   the expected fallback path.

## Expected

- The workflow completes successfully.
- `manifest.json`, `run.json`, and `summary.md` exist in the artifact.
- A PR is created in `holon-run/holon-test`.
- If broker exchange succeeds, GitHub side effects should be authored by
  `holonbot[bot]`; otherwise the artifact/log should make the fallback clear.

## Pass / Fail Criteria

- Pass:
  - Workflow exits 0.
  - Artifact exists.
  - PR URL is present in GitHub state, `summary.md`, or `run.json`.
- Fail (`infra-fail`):
  - OIDC token request, broker exchange, checkout, provider auth, or workflow
    execution fails.
- Fail (`agent-fail`):
  - Runtime completes but no PR is created for the issue.

## Evidence to Capture

- Issue URL.
- Workflow run URL.
- PR URL.
- `holon-solve-output` artifact.
- Relevant token-resolution log lines.
