# run-pptx-remote-skill

## Purpose

Validate `holon run` end-to-end with:

- local agent bundle (built from current workspace)
- automatic agent-home initialization
- remote non-code skill activation via `--skills`
- concrete artifact generation (`.pptx`)

This case focuses on productized run reliability rather than code changes.

## Preconditions

- `bin/holon` exists (`make build` if missing).
- Docker daemon is running.
- `gh` is installed (for remote skill fetch via GitHub path resolver).
- Anthropic credentials are available either:
  - env vars: `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL`
  - or `~/.claude/settings.json` with `.env` entries.
- Network is available (required for remote skill download and model calls).

## Steps

1. Build local agent bundle from current source tree.
2. Run `holon run` with:
   - empty/new `--agent-home`
   - `--agent <local bundle path>`
   - `--skills ghpath:anthropics/skills/skills/pptx@main`
3. Ask agent to create a short PPT file in workspace.
4. Collect logs and artifacts.

Use `run.sh` for scripted execution.

## Expected

- Run exits successfully.
- Agent home is auto-initialized.
- A `.pptx` file is created under workspace.
- Run logs show selected local bundle path and remote skill resolution.

## Pass / Fail Criteria

- Pass:
  - `holon run` returns exit code `0`
  - at least one `*.pptx` exists in workspace
  - `agent-home/agent.yaml` exists
- Fail (`infra-fail`):
  - local bundle build fails
  - remote skill cannot be downloaded/resolved
  - runtime/container startup fails
- Fail (`agent-fail`):
  - run succeeds but no PPT artifact is produced
  - produced file is empty or obviously invalid

## Evidence to Capture

- run log (`run.log`)
- run metadata (`run-meta.env`)
- workspace file list
- agent-home file list
- generated `.pptx` artifact

Use `collect.sh` to gather evidence into `artifacts/`.

## Known Failure Modes

- Missing Anthropic token/base URL.
- Remote GitHub path ref unavailable or rate-limited.
- Model completes without creating output file (instruction-following miss).
