# RFC Status Index

This folder contains both active specifications and historical design drafts.

## Status Definitions

- `Active`: current contract/design reference.
- `Draft`: design in progress; not guaranteed to match current implementation.
- `Superseded`: replaced by newer RFC(s); kept for historical context.
- `Deprecated`: no longer recommended and not planned for further evolution.

## Current Status

- `0001-holon-atomic-execution-unit.md`: `Superseded` (baseline historical model; replaced by newer contract direction)
- `0002-agent-scheme.md`: `Active` (agent contract baseline)
- `0003-skill-artifact-architecture.md`: `Active` (skill-first artifact/runtime direction)
- `0004-proactive-agent-controller.md`: `Draft` (serve/controller design evolving)
- `0005-serve-api-direction.md`: `Draft` (control-plane direction; not fully implemented)
- `0006-autonomous-capability-composition.md`: `Draft` (capability composition model)

## Maintenance Rule

When behavior changes in runtime/path/session semantics:
1. update `README.md` and relevant `docs/` pages;
2. update affected RFC status and "implementation reality" notes;
3. keep historical content, but mark outdated assumptions explicitly.
