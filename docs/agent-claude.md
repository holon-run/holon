# Claude Agent (Reference Implementation)

This document is **non-normative** and describes the current Claude agent implementation. The **normative agent contract** is defined in `rfc/0002-agent-scheme.md`.

## Implementation location
- Agent sources: `agents/claude/src/agent.ts`
- Agent bundle: `agents/claude/dist/agent-bundles/*.tar.gz`
- Entrypoint (inside composed image): `/holon/agent/bin/agent`

## Underlying engine
The agent drives Claude Code behavior headlessly via the Claude Agent SDK:
- SDK: `@anthropic-ai/claude-agent-sdk`
- Claude Code runtime: installed in the composed runtime image

## Execution image composition
Holon composes a final execution image (at run time) from:
- **Base toolchain image** (`--image`, e.g. `golang:1.22`)
- **Agent bundle** (`--agent`, a `.tar.gz` archive)

The composed image installs required tooling (Node, git, `gh`) and the Claude Code runtime, then uses the agent bundle entrypoint.

## Container filesystem layout
The agent expects the standard Holon layout:
- Workspace (snapshot): `HOLON_WORKSPACE_DIR` (typically `/root/workspace`, runner sets this as `WorkingDir`)
- Inputs:
  - `${HOLON_INPUT_DIR}/spec.yaml`
  - `${HOLON_INPUT_DIR}/context/` (optional)
  - `${HOLON_INPUT_DIR}/prompts/system.md` and `${HOLON_INPUT_DIR}/prompts/user.md` (optional)
- Outputs:
  - `${HOLON_OUTPUT_DIR}/manifest.json`
  - `${HOLON_OUTPUT_DIR}/diff.patch` (when requested)
  - `${HOLON_OUTPUT_DIR}/summary.md` (when requested)
  - `${HOLON_OUTPUT_DIR}/evidence/` (optional)

## Headless / non-interactive behavior
The agent must run without a TTY and must not block on prompts:
- Pre-seed Claude Code config as needed (e.g. `~/.claude/*`) inside the image/layer.
- Force an explicit permission mode appropriate for sandbox execution.
- Fail fast if required credentials are missing, and record details in `manifest.json`.

## Patch generation
When `diff.patch` is required, the agent generates a patch that the runner/workflows can apply using `git apply`:
- If the workspace is already a git repo, use `git diff`.
- If not, initialize a temporary git repo inside the snapshot for baseline+diff.

For binary compatibility, prefer `git diff --binary --full-index`.

## Configuration knobs
Common environment variables:
- Model: `HOLON_MODEL`, `HOLON_FALLBACK_MODEL`
- Timeouts/health: `HOLON_QUERY_TIMEOUT_SECONDS`, `HOLON_HEARTBEAT_SECONDS`, `HOLON_RESPONSE_IDLE_TIMEOUT_SECONDS`, `HOLON_RESPONSE_TOTAL_TIMEOUT_SECONDS`

See `agents/claude/README.md` for the full list.
