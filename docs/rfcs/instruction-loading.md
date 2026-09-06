---
title: RFC: Instruction Loading
date: 2026-04-21
status: accepted
issue:
  - 64
  - 68
  - 2793
---

# RFC: Instruction Loading

## Summary

Holon should define instruction loading independently from shell cwd. The
phase-1 contract is:

- user-global instructions load from `<user_home>/.agents/AGENTS.md`
- agent-scoped instructions load from `<agent_home>/AGENTS.md`
- workspace-scoped instructions load from `<workspace_anchor>/AGENTS.md`
- `CLAUDE.md` is a fallback only when workspace `AGENTS.md` is absent
- hierarchical loading below `workspace_anchor` is not the default behavior

## Why

Without a stable loading contract, daemon startup cwd, worktree entry, and
shell `cd` can all accidentally redefine instructions. That makes long-lived
runtime behavior hard to inspect and reason about.

## Root Selection

### User-Global Scope

User-global scope is rooted at the operating-system user home captured when the
daemon configuration is loaded. It is not `HOLON_HOME`, `AppConfig.home_dir`,
the shell cwd, the workspace anchor, or a provider capability.

The captured `user_home` is passed into each runtime independently of whether
the runtime uses a static or reconfigurable provider. Config reloads preserve
the runtime's original user home so an environment change cannot redefine
instructions between turns.

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
2. user-global `AGENTS.md`
3. agent-scoped `AGENTS.md`
4. workspace-scoped `AGENTS.md`
5. activated skills
6. dynamic runtime attachments

The default contract is intentionally simple and inspectable.

Turn-scoped operator instructions remain later and therefore more specific
than these stable instruction roots.

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
- whether they came from user-global, agent, or workspace scope
- which path won when `AGENTS.md` and `CLAUDE.md` were both possible
- `not_found` when a known root does not contain the candidate file
- `root_unavailable` when the runtime mode has no root for that scope
- `not_evaluated` when a storage-only summary intentionally avoids file I/O

Unreadable files and decoding failures are context-build errors. They should
include the instruction scope and candidate path and must not be silently
treated as absent.

## Startup and Inspection Modes

Normal operator/external turns, recovered runtimes, prompt preview, public
named agents, and private child agents use the same captured user-global root.
A detached runtime projects its agent home as the active workspace root, so
workspace status describes that explicit projection; detachment does not
disable user-global or agent-scoped loading.

Offline runtimes may omit user home when they do not build provider prompts.
Storage-only summaries do not read instruction files and report
`not_evaluated` instead of claiming that files are missing.

## Related Historical Notes

Supersedes and absorbs the instruction-loading portions of:

- `docs/archive/workspace-binding-and-instruction-loading.md`
