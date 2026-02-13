# `holon run` Safety Release Checklist

Use this checklist before cutting a release candidate that claims `holon run` stability.

## 1. Sandbox boundary checks

- Input mount is read-only (`/input`).
- Workspace and output mounts are read-write (`/workspace`, `/output`).
- No privileged container settings are enabled.
- Runtime host config keeps default isolated network mode and no PID namespace sharing.

## 2. Output contract checks

- Runtime injects `HOLON_OUTPUT_DIR`.
- Skills/tests write required artifacts via `HOLON_OUTPUT_DIR` (no hardcoded `/output` dependency).
- `${HOLON_OUTPUT_DIR}/manifest.json` is produced for non-skill-first flows.
- Missing required artifacts fail with diagnosable errors.

## 3. Secret-handling checks

- `redactLogs` is exercised in regression tests.
- Sensitive patterns in execution logs are redacted.
- Redaction failures are visible in logs and do not silently corrupt artifacts.

## 4. CI gate checks

- `test-run-safety` job passes in CI.
- Integration suite includes output env contract coverage.
- No failing runtime/docker safety tests on the release branch.

## 5. Manual spot-check (recommended)

- Run a canary `holon run` against a small repo with `--log-level debug`.
- Verify artifacts and diagnostics under the output directory are sufficient for triage.
