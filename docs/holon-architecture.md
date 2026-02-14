# Holon Architecture (Design Notes)

This document is **non-normative**. It explains *why* Holon is structured the way it is and how the pieces fit together. For the normative protocol/contract, see:
- `rfc/0001-holon-atomic-execution-unit.md`
- `rfc/0002-agent-scheme.md` (agent contract)

## Goals (design intent)
- Treat an AI agent run like a **batch job** with explicit inputs/outputs.
- Keep agents **platform-agnostic** (no embedded GitHub/Jira logic).
- Make CI integration **deterministic** by relying on standard artifacts (`diff.patch`, `summary.md`, `manifest.json`).

## High-level architecture
Holon is split into:
- **Runner (holon CLI)**: orchestrates container execution and validates outputs.
- **Agent (in container)**: bridges Holon contract to a specific engine/runtime (Claude Code, Codex, …).

Typical flow:
1) Runner prepares `HOLON_INPUT_DIR` and a workspace snapshot mounted at `HOLON_WORKSPACE_DIR` (typically `/root/input` and `/root/workspace`).
2) Runner runs a composed image that includes the agent bundle.
3) Agent reads inputs, drives the underlying engine, and writes artifacts to `HOLON_OUTPUT_DIR` (typically `/root/output`).
4) Runner (or external publisher) uploads/publishes artifacts (e.g. apply patch, open PR) via workflows.

## Why “patch-first”
Holon’s default integration boundary is a patch file (`diff.patch`) because it enables:
- explicit human review (`git apply --3way`),
- easy PR updates in CI,
- agent/engine neutrality (not every tool supports native “create PR”).

## Why “context injection”
Holon keeps context as explicit inputs under `${HOLON_INPUT_DIR}/context/`.

- For `holon run`, the caller/workflow injects context directly.
- For `holon solve`, skill workflows collect GitHub context and write it under the same input contract.

This keeps runs auditable (context is part of execution record) and preserves agent/runtime neutrality.

## Image composition (Build-on-Run)
Many tasks need a project toolchain (Go/Node/Java/etc.). Holon supports composing an execution image at run time:
- **Base image**: toolchain (e.g. `golang:1.22`, `node:20`)
- **Agent bundle**: agent bridge + dependencies

This avoids maintaining a large prebuilt agent×toolchain matrix.

## Related docs
- `docs/operator-guide-v0.11.md`: operator-facing boundary for stable (`run`/`solve`) vs preview (`serve`) surfaces.
- `docs/modes.md`: skill-first architecture and CLI behavior.
- `docs/agent-encapsulation.md`: non-normative description of the agent pattern and image composition approach.
- `docs/agent-claude.md`: reference implementation notes for the Claude agent.
