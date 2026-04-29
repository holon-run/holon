---
title: RFC: Agent Home Directory Layout
date: 2026-04-28
status: draft
---

# RFC: Agent Home Directory Layout

## Summary

Holon should define `agent_home` as both an agent-owned workspace and a
runtime-owned state root with explicit directory ownership boundaries.

The phase-1 layout is:

```text
agent_home/
  AGENTS.md
  memory/
    self.md
    operator.md
  notes/
  work/
  skills/
  .holon/
    state/
    ledger/
    indexes/
    cache/
```

Visible top-level files and directories are the agent-maintained workspace
surface. `.holon/` is runtime-owned state and must not be treated as ordinary
agent notes or Markdown memory.

## Context

Existing RFCs already define parts of `agent_home`:

- `agent-initialization-and-template.md` defines `agent_home` as the agent
  identity root.
- `instruction-loading.md` defines `agent_home/AGENTS.md` as the
  agent-scoped instruction source.
- `workspace-entry-and-projection.md` defines `AgentHome` as the non-removable
  fallback workspace.
- `agent-and-workspace-memory.md` defines the phase-1 agent memory files under
  `agent_home/memory/`.

Those documents do not yet define the filesystem contract for the directory as
a whole. Without that contract, agent-authored files, runtime JSONL state,
memory projections, notes, skills, and cache files can drift into one
ambiguous namespace.

## Goals

- Keep `agent_home` usable as the agent's default workspace.
- Keep runtime-owned state separate from agent-authored files.
- Make it clear which paths an agent may edit directly.
- Provide a stable home for `AGENTS.md`, agent memory, notes, work artifacts,
  and agent-local skills.
- Provide a stable hidden home for runtime state, ledgers, indexes, and cache.
- Keep the phase-1 layout small enough to inspect manually.

## Non-Goals

- This RFC does not define the exact schema of runtime JSONL records.
- This RFC does not define a full backup or migration system.
- This RFC does not define workspace-scoped memory layout for external project
  workspaces.
- This RFC does not define the final `MemorySearch` or `MemoryGet` tool
  contract.
- This RFC does not require every path to exist eagerly at agent creation time.

## Core Principles

### `agent_home` has two ownership zones

The visible top-level zone is agent-authored:

- `AGENTS.md`
- `memory/`
- `notes/`
- `work/`
- `skills/`

The hidden `.holon/` zone is runtime-owned:

- `.holon/state/`
- `.holon/ledger/`
- `.holon/indexes/`
- `.holon/cache/`

An agent may read runtime-owned state through approved tools or debug
surfaces, but ordinary file editing should target the visible workspace zone.

### `AgentHome` is a workspace, not a project substitute

Every agent starts with `AgentHome` as its active workspace. This gives file
tools, shell tools, checkpoints, and finalization a stable root even before an
external workspace is attached.

`AgentHome` is suitable for:

- agent-local notes;
- durable agent memory;
- scratch work that is not project-local;
- agent-local skills;
- runtime state owned by Holon.

It is not a substitute for a project workspace. Project code, project docs,
project-specific rules, and project-specific memory should stay associated
with the relevant `workspace_id`.

### Loaded guidance stays narrow

`agent_home/AGENTS.md` is automatically loaded and should remain a concise
guidance file. It is not a dumping ground for history, raw notes, or large
memory summaries.

Content should only be promoted into `AGENTS.md` when it must shape the agent's
behavior on every run.

### Runtime state is not Markdown memory

Episode records, briefs, work items, transcript entries, tool execution
records, indexes, and cache files are runtime evidence or derived state. They
belong under `.holon/` or another runtime-owned store, not under
`agent_home/memory/`.

Markdown memory may summarize durable lessons, but the stronger source of
truth remains the runtime ledger or the relevant workspace artifact.

## Phase-1 Layout

### `AGENTS.md`

`agent_home/AGENTS.md` is the agent-local instruction file.

It should contain:

- role definition;
- responsibilities;
- durable collaboration rules;
- operating habits that should apply every time;
- agent-specific skill expectations.

It should not contain:

- current task progress;
- raw conversation history;
- long experience logs;
- workspace-specific project facts that belong to a project workspace;
- runtime state copied out of `.holon/`.

### `memory/`

`agent_home/memory/` contains curated agent-scoped Markdown memory.

Phase 1 has two files:

```text
memory/
  self.md
  operator.md
```

`memory/self.md` is the agent's self memory: role lessons, recurring mistakes,
cross-workspace workflow habits, capability notes, and operating preferences
that are useful across workspaces but too detailed for `AGENTS.md`.

`memory/operator.md` is the agent's durable model of the operator: stable
preferences, collaboration patterns, autonomy expectations, communication
style, and cross-workspace rules that should not be duplicated into every
workspace profile.

These files should be searched or selectively loaded. They should not be
injected wholesale by default unless they remain small enough for the prompt
budget.

Phase 1 should not split memory by date. Date-oriented records belong in notes,
work items, episodes, or runtime ledgers. If `self.md` or `operator.md` become
too large, a future RFC may introduce `memory/topics/*.md`.

### `notes/`

`agent_home/notes/` is the agent's ordinary note space.

It may contain:

- investigation notes;
- drafts;
- scratch summaries;
- temporary research;
- human-readable handoff notes that are not yet durable memory.

Notes may be searched, but they do not carry the same semantic weight as
`memory/`. A note is not automatically a stable preference, rule, or lesson.

### `work/`

`agent_home/work/` is for non-project-local work artifacts.

Examples:

- generated reports that are not owned by a project workspace;
- temporary plans;
- local experiments;
- operator-requested files that do not belong in an attached workspace.

When work clearly belongs to an external project, the agent should use that
project workspace instead of storing the artifact in `AgentHome`.

### `skills/`

`agent_home/skills/` is the agent-local skill surface.

It may contain copied skills, linked skills, or skill references, depending on
the skill discovery implementation. The important contract is that these
skills are scoped to the agent identity rather than user-global or
workspace-wide.

### `.holon/state/`

`.holon/state/` stores current runtime state that is not append-only evidence.

Examples may include:

- active work item state;
- waiting state;
- attached workspace bindings;
- profile snapshots;
- consolidation job state.

Agents should not edit these files directly. Runtime tools and control-plane
operations own the schema and write path.

### `.holon/ledger/`

`.holon/ledger/` stores append-only durable evidence.

Examples may include:

- messages;
- briefs;
- tool execution records;
- context episodes;
- work item transitions;
- memory candidate records.

This data may be indexed and summarized, but ordinary Markdown memory should
not replace it.

### `.holon/indexes/`

`.holon/indexes/` stores derived search indexes.

Indexes must be rebuildable from stronger sources such as ledgers, memory
files, workspace profiles, and instruction files. They should not be treated
as the source of truth.

### `.holon/cache/`

`.holon/cache/` stores disposable runtime cache.

Cache content may speed up execution, provider calls, discovery, or search,
but Holon should be able to delete and rebuild it without losing durable
memory or evidence.

## Creation Rules

Agent creation should ensure that `agent_home` exists and may seed:

- `AGENTS.md`, composed from the selected agent template plus Holon's required
  agent-home directory guidance;
- `memory/self.md`, if the template includes initial self memory;
- `memory/operator.md`, if an operator profile is available;
- `skills/`, if the template includes agent-local skills;
- `.holon/`, when the runtime first needs state, ledger, index, or cache
  storage.

Holon does not need to create every optional visible directory eagerly. Lazy
creation is acceptable as long as the ownership rule remains stable.

### Required directory guidance

Holon must not rely on every template author to remember the `agent_home`
layout rules.

Phase 1 should use automatic composition. Agent initialization should render
`agent_home/AGENTS.md` once from:

```text
selected template guidance
+ Holon required agent-home directory guidance
+ optional operator/profile seed
```

This is not a post-initialization patch. The required guidance is appended or
inserted by the initialization renderer while materializing the initial
`AGENTS.md`.

Templates do not need to include a special placeholder for the required
guidance in phase 1. A future template format may allow explicit placement,
but the renderer must still guarantee that the required guidance is present.

A template may add role-specific detail or stricter habits, but a template
that omits directory maintenance rules should still produce an `AGENTS.md`
that explains:

- `agent_home` is the agent's default workspace;
- `AGENTS.md` is the automatically loaded agent guidance file;
- `memory/self.md` and `memory/operator.md` are curated agent-scoped memory;
- `notes/` is ordinary working notes;
- `work/` is for non-project-local work artifacts;
- `skills/` is for agent-local skills;
- `.holon/` is runtime-owned and should not be edited as ordinary files;
- project-specific files, rules, and memory belong to the active project
  workspace rather than `AgentHome`.

This fragment is part of Holon's initialization contract, not an optional
template convention.

### Template customization

Templates should customize how a role uses the visible agent-owned area. For
example, a reviewer template may say what review lessons belong in
`memory/self.md`, while a release template may define what release handoff
notes belong in `notes/`.

Templates should not redefine `.holon/` as agent-editable state or move
runtime ledgers into Markdown memory. Runtime ownership boundaries remain
defined by this RFC and by the base runtime instructions.

## Editing Rules

Agents may directly edit:

- `AGENTS.md`, when updating their durable behavior is appropriate;
- `memory/self.md`;
- `memory/operator.md`;
- files under `notes/`;
- files under `work/`;
- agent-local skill files or references under `skills/`, subject to skill
  policy.

Agents should not directly edit:

- `.holon/state/`;
- `.holon/ledger/`;
- `.holon/indexes/`;
- `.holon/cache/`.

If an agent needs to affect runtime state, it should use the relevant runtime
tool or control-plane operation instead of patching hidden state files.

## Prompt And Search Behavior

Base runtime instructions should enforce the non-negotiable directory
boundaries even if an existing `agent_home/AGENTS.md` is missing, stale, or
manually edited. The base prompt should stay short and focus on ownership and
safety:

- `AgentHome` is a workspace, but not a project workspace substitute;
- `.holon/` is runtime-owned;
- project-scoped work should go to the active project workspace;
- `AGENTS.md` and `memory/` have different loading and recall semantics.

Default prompt projection should load:

- runtime/base instructions;
- `agent_home/AGENTS.md`;
- active workspace instructions;
- selected runtime context.

Default prompt projection should not load all of:

- `memory/self.md`;
- `memory/operator.md`;
- `notes/`;
- `work/`;
- `.holon/ledger/`.

Memory and notes should be retrieved through a recall/search path. Runtime
state and ledger records should be projected through bounded summaries or
exact lookup tools.

## Relationship To External Workspaces

External project workspaces have their own `workspace_id` and
`workspace_anchor`.

When the active workspace is external:

- project files should be edited under that workspace's execution root;
- workspace instructions should load from the workspace anchor;
- workspace-scoped memory should be keyed by the external `workspace_id`;
- `agent_home` remains available for agent-scoped memory, notes, skills, and
  runtime state.

Returning to `AgentHome` should not detach or forget external workspaces. It
only changes the active workspace back to the agent's local root.

## Open Questions

- Should `.holon/` live physically under `agent_home`, or should it be a
  logical path backed by a host-level runtime store?
- Which visible directories should be created eagerly by templates?
- Should agent-created files under `notes/` and `work/` be indexed by default?
- What migration path should Holon use if older agents already have runtime
  files mixed into the visible root?
- Should runtime-owned paths be hidden from normal model-facing file listings
  unless explicitly requested?

## Relationship To Other RFCs

- `agent-initialization-and-template.md` defines how `agent_home` is created.
- `instruction-loading.md` defines how `agent_home/AGENTS.md` is loaded.
- `workspace-entry-and-projection.md` defines `AgentHome` as the fallback
  active workspace.
- `agent-and-workspace-memory.md` defines agent memory and workspace memory
  boundaries.
- This RFC defines the filesystem layout and ownership boundaries inside
  `agent_home`.
