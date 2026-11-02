---
title: RFC: Instruction Loading
date: 2026-04-21
status: accepted
issue:
  - 64
  - 68
  - 2793
  - 3889
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

## Same-File Agent and Workspace Guidance

After selecting and reading all sources, the loader omits workspace guidance
when its selected file is the same file as agent guidance and both reads contain
identical text. The agent source and its original display path remain unchanged;
workspace source becomes absent with status `duplicate_of_agent_scope`. Prompt
rendering, preview, and summaries consume this same loader decision.

File identity uses equal canonical paths, or on Unix equal device and inode
metadata (following symlinks). This covers symlink aliases and Unix hard links
without adding a dependency. Non-Unix hard-link equivalence beyond canonical
paths is not guaranteed. Identity probing is best-effort: failure to establish
identity retains both sources without adding context-build errors. Equal text
alone never merges independent files; differing reads of the same file are
also retained. This is not an atomic file snapshot or an authorization check.

Selection and read-error behavior are unchanged. A duplicate `AGENTS.md` does
not trigger `CLAUDE.md` fallback, but an already-selected `CLAUDE.md` pointing to
the agent file can itself be omitted. Missing agent guidance does not disable
workspace fallback. User-global guidance is outside this deduplication scope.
Both files may still be read; the saved cost is duplicate prompt injection.

The new status is an observable wire-enum extension. Existing JSON fields,
defaults, legacy aliases, and content omission remain unchanged; old strict
enum clients may need updating to accept the new value. No database migration
is needed.

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
- `duplicate_of_agent_scope` when workspace guidance is included in agent scope

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
