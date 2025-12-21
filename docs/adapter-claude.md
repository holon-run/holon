# Claude Adapter (Reference Implementation)

This document is **non-normative** and describes the current Claude adapter implementation. The **normative adapter contract** is defined in `rfc/0002-adapter-scheme.md`.

## Implementation location
- Adapter sources: `images/adapter-claude/src/adapter.ts`
- Agent bundle: `images/adapter-claude/dist/agent-bundles/*.tar.gz`
- Entrypoint (inside composed image): `/holon/agent/bin/agent`

## Underlying runtime
The adapter drives Claude Code behavior headlessly via the Claude Agent SDK:
- SDK: `@anthropic-ai/claude-agent-sdk`
- Claude Code runtime: installed in the composed runtime image

## Runtime image composition
Holon composes a final runtime image (at run time) from:
- **Base toolchain image** (`--image`, e.g. `golang:1.22`)
- **Agent bundle** (`--agent-bundle`, a `.tar.gz` archive)

The composed image installs required tooling (Node, git, `gh`) and the Claude Code runtime, then uses the agent bundle entrypoint.

## Container filesystem layout
The adapter expects the standard Holon layout:
- Workspace (snapshot): `/holon/workspace` (host sets this as `WorkingDir`)
- Inputs:
  - `/holon/input/spec.yaml`
  - `/holon/input/context/` (optional)
  - `/holon/input/prompts/system.md` and `/holon/input/prompts/user.md` (optional)
- Outputs:
  - `/holon/output/manifest.json`
  - `/holon/output/diff.patch` (when requested)
  - `/holon/output/summary.md` (when requested)
  - `/holon/output/evidence/` (optional)

## Headless / non-interactive behavior
The adapter must run without a TTY and must not block on prompts:
- Pre-seed Claude Code config as needed (e.g. `~/.claude/*`) inside the image/layer.
- Force an explicit permission mode appropriate for sandbox execution.
- Fail fast if required credentials are missing, and record details in `manifest.json`.

## Patch generation
When `diff.patch` is required, the adapter generates a patch that the host/workflows can apply using `git apply`:
- If the workspace is already a git repo, use `git diff`.
- If not, initialize a temporary git repo inside the snapshot for baseline+diff.

For binary compatibility, prefer `git diff --binary --full-index`.

## Configuration knobs
Common environment variables:
- Model: `HOLON_MODEL`, `HOLON_FALLBACK_MODEL`
- Timeouts/health: `HOLON_QUERY_TIMEOUT_SECONDS`, `HOLON_HEARTBEAT_SECONDS`, `HOLON_RESPONSE_IDLE_TIMEOUT_SECONDS`, `HOLON_RESPONSE_TOTAL_TIMEOUT_SECONDS`

See `images/adapter-claude/README.md` for the full list.
