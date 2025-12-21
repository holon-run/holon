# Holon Architecture (Design Notes)

This document is **non-normative**. It explains *why* Holon is structured the way it is and how the pieces fit together. For the normative protocol/contract, see:
- `rfc/0001-holon-atomic-execution-unit.md`
- `rfc/0002-adapter-scheme.md`

## Goals (design intent)
- Treat an AI agent run like a **batch job** with explicit inputs/outputs.
- Keep adapters **platform-agnostic** (no embedded GitHub/Jira logic).
- Make CI integration **deterministic** by relying on standard artifacts (`diff.patch`, `summary.md`, `manifest.json`).

## High-level architecture
Holon is split into:
- **Host (holon CLI)**: orchestrates container execution and validates outputs.
- **Adapter (in container)**: bridges Holon contract to a specific tool/runtime (Claude Code, Codex, …).

Typical flow:
1) Host prepares `/holon/input` and a workspace snapshot mounted at `/holon/workspace`.
2) Host runs the adapter image (or a composed image).
3) Adapter reads inputs, drives the underlying tool, and writes artifacts to `/holon/output`.
4) Host uploads/publishes artifacts (e.g. apply patch, open PR) via workflows.

## Why “patch-first”
Holon’s default integration boundary is a patch file (`diff.patch`) because it enables:
- explicit human review (`git apply --3way`),
- easy PR updates in CI,
- adapter/tool neutrality (not every tool supports native “create PR”).

## Why “context injection”
Holon does not fetch issue/PR context itself. The caller (workflow/local script) injects context files under `/holon/input/context/` so:
- adapters remain tool/platform-agnostic,
- runs are auditable (context is part of the execution record),
- workflows can decide what to include (issue body, linked issues, diffs, logs, etc.).

## Image composition (Build-on-Run)
Many tasks need a project toolchain (Go/Node/Java/etc.). Holon supports composing a runtime image at execution time:
- **Base image**: toolchain (e.g. `golang:1.22`, `node:20`)
- **Adapter layer**: adapter bridge + underlying agent runtime

This avoids maintaining a large prebuilt adapter×toolchain matrix.

## Related docs
- `docs/modes.md`: design for `mode` as the single user-facing selector (execute/plan/review).
- `docs/adapter-encapsulation.md`: non-normative description of the adapter pattern and image composition approach.
- `docs/adapter-claude.md`: reference implementation notes for the Claude adapter.
