# Workspace test migration script boundary

## Decision

Keep `scripts/migrate_workspace_tests.sh` as a retained, historical migration
helper. Do not invoke it from CI, release automation, or the normal test
entrypoints, and do not delete it based only on the absence of repository-local
callers.

## Evidence

- The script is not referenced by the current repository's tracked workflows,
  Makefile, scripts, tests, or documentation.
- Its input path, `tests/support/runtime_flow.rs`, is no longer present on the
  current `main` branch.
- Its output, `tests/support/runtime_workspace_worktree.rs`, remains an active
  test-support module imported by `tests/runtime_workspace_worktree.rs` and
  `tests/support/mod.rs`.
- Git history shows the script was added as a one-time extraction aid in
  commit `8a3ac385`; no later commit adds a supported invocation path.

## Preserved boundary

The absence of repository-local consumers is evidence that the script is not a
maintained test or release entrypoint, not proof that no checkout, fork, or
operator script uses it. Future removal requires a separate deprecation window
or explicit confirmation that external consumers have migrated. Changes to the
generated test-support module must use the normal source and test review path,
not this stale migration helper.
