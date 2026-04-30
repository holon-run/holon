# E2E Manual

This directory stores manual end-to-end test cases for Holon.

These cases are documentation-style tests: they define the setup, trigger,
expected behavior, and evidence to collect for workflows that depend on live
GitHub repositories, tokens, external model providers, and hosted services.

## Structure

- `CASE.md`: Human-readable test case definition and pass/fail criteria.
- `run.sh`: Optional helper script for the live flow.
- `collect.sh`: Optional helper script to collect logs and artifacts.
- `artifacts/`: Optional local evidence directory, ignored by git.

## Current Cases

- `holon-test-issue-resolve`: Validate `holon solve` can resolve a live
  `holon-run/holon-test` issue through the GitHub Actions reusable workflow and
  publish a PR.
- `holon-test-review-pr`: Validate `holon solve` can review a live
  `holon-run/holon-test` PR through the GitHub Actions reusable workflow and
  publish a review/comment.
- `holon-test-fix-pr`: Validate `holon solve` can fix a live
  `holon-run/holon-test` PR through the GitHub Actions reusable workflow and
  push a follow-up commit.

## Failure Classification

- `infra-fail`: GitHub Actions, auth, checkout, token broker, model provider,
  or runtime startup failed.
- `agent-fail`: The workflow and runtime completed, but the agent missed the
  expected GitHub side effect.
