# `holon run` GA Contract (v0.11)

This document defines the compatibility contract for `holon run` in v0.11.

## Goal

`holon run` is the stable execution kernel:
- run an agent safely in sandboxed runtime,
- accept explicit run-time inputs,
- produce deterministic machine-readable execution results.

## Stable (GA) Contract Surface

### 1. Runtime and isolation boundary

- Execution happens in containerized sandbox runtime.
- Runtime controls container mounts and environment injection.
- Agent/skill execution is isolated from host by container boundary.

### 2. Input/output contract

- Runtime provides input, workspace, and output boundaries to agent.
- Runtime MUST expose output boundary via `HOLON_OUTPUT_DIR`.
- Default output location is agent-home-scoped (for example, `${agent_home}/runs/<run_id>/output` on host), but the concrete container mount path is an implementation detail.
- Agents/skills SHOULD write outputs by environment contract (`HOLON_OUTPUT_DIR`) and SHOULD NOT rely on hardcoded paths like `/output`.
- `manifest.json` is the required execution record output.
- Optional artifacts are skill-defined and enumerated via manifest.

### 3. Skill activation semantics

`--skill`/`--skills` are run-time activation inputs, not lifecycle installation commands.

- `holon run --skill/--skills` means: "enable these skills for this run".
- Resolution precedence is stable:
  - CLI (`--skill`, `--skills`)
  - project config
  - spec metadata
  - auto-discovered workspace skills
- CLI skills have highest precedence but do not disable lower-precedence sources.

### 4. Default agent-home behavior

- `holon run` without `--agent-id`/`--agent-home` uses ephemeral agent-home by default.
- `holon run` with `--agent-id` or `--agent-home` uses persistent agent-home.
- `holon serve` default remains persistent `main` agent-home.

### 5. Error/exit behavior

- Invalid inputs and contract violations return explicit errors.
- Missing required outputs (for active contract checks) are surfaced deterministically.

## Explicitly Non-GA / Experimental

- `holon serve` control-plane and ingress protocol evolution.
- TUI interaction model and RPC method evolution.
- External subscription transport strategy and connectors.
- Agent-level skill management UX beyond run-time activation.

## Notes for Operators

- Use `holon run` for stable one-shot execution.
- Use `holon solve` as higher-level wrapper over `run`.
- Treat `holon serve` as preview/experimental in v0.11.
- Use `docs/run-safety-checklist.md` as release gating checklist for runtime safety regressions.
