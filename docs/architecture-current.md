# Holon Current Architecture Baseline

## Product Layers

1. Execution Layer: `holon run` (stable)
2. Workflow Layer: `holon solve` (stable wrapper on `run`)
3. Control Layer: `holon serve` (experimental proactive runtime)

## Core Runtime Model

Holon executes agents in a sandboxed container runtime and centers persistence on `agent_home`.

`agent_home` holds:
- agent identity/persona files (`AGENTS.md`, `ROLE.md`, `IDENTITY.md`, `SOUL.md`, `CLAUDE.md`)
- runtime state
- caches
- workspace/output data managed by runtime

## Contract Boundaries

- Inputs: runtime-provided request/context envelope.
- Workspace: runtime-provided working directory (mode-dependent).
- Outputs: runtime-recommended output directory with `manifest.json` as minimal execution record.

Contract consumers should rely on documented runtime variables and behavior, not hardcoded `/holon/*` paths.

## Mode Semantics

### `holon run`
- one-shot, deterministic sandbox run
- primary reliability target for release readiness

### `holon solve`
- GitHub-focused convenience command on top of `run`
- preserves issue/pr workflow expectations while reusing runtime contract

### `holon serve`
- persistent runtime handling events and message-driven interactions
- session/control-plane behavior is still evolving and treated as experimental

## Documentation Sources

- Public entry: `README.md`
- Contributor rules: `AGENTS.md`
- Contract-level specs: `rfc/*.md` with status index in `rfc/README.md`
