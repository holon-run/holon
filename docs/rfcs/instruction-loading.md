---
title: RFC: Instruction Loading
date: 2026-04-21
status: accepted
issue:
  - 64
  - 68
---

# RFC: Instruction Loading

## Summary

Holon should define instruction loading independently from shell cwd. The
phase-1 contract is:

- agent-scoped instructions load from `<agent_home>/AGENTS.md`
- workspace-scoped instructions load from `<workspace_anchor>/AGENTS.md`
- `CLAUDE.md` is a fallback only when workspace `AGENTS.md` is absent
- hierarchical loading below `workspace_anchor` is not the default behavior

## Why

Without a stable loading contract, daemon startup cwd, worktree entry, and
shell `cd` can all accidentally redefine instructions. That makes long-lived
runtime behavior hard to inspect and reason about.

## Root Selection

### Agent Scope

Agent scope is rooted at `agent_home`. This is stable across workspaces and
persists with the agent.

### Workspace Scope

Workspace scope is rooted at `workspace_anchor`, not the shell's transient cwd.
This keeps project-level guidance stable even when execution moves into a
subdirectory or worktree projection.

## Phase-1 Default Behavior

Holon should load:

1. runtime/base instructions
2. agent-scoped `AGENTS.md`
3. workspace-scoped `AGENTS.md`
4. activated skills
5. dynamic runtime attachments

The default contract is intentionally simple and inspectable.

## `CLAUDE.md` Compatibility

Workspace `CLAUDE.md` should only be considered when workspace `AGENTS.md` is
absent. Holon should not treat both as co-equal primary roots.

## Hierarchical Loading

Optional hierarchical `AGENTS.md` loading from `workspace_anchor` down to the
runtime `cwd` remains future work. It should stay opt-in unless later evidence
shows it improves coding flows without destabilizing workspace identity.

## Inspectability

Prompt and debug surfaces should be able to report:

- which instruction sources were loaded
- whether they came from agent or workspace scope
- which path won when `AGENTS.md` and `CLAUDE.md` were both possible

## Related Historical Notes

Supersedes and absorbs the instruction-loading portions of:

- `docs/archive/workspace-binding-and-instruction-loading.md`
