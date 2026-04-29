---
title: RFC: Agent and Workspace Memory
date: 2026-04-28
status: draft
---

# RFC: Agent and Workspace Memory

## Summary

Holon should define memory around durable identity scopes rather than around
provider sessions.

The first memory model has four primary surfaces:

1. `agent_home/AGENTS.md` is the agent's loaded identity and operating memory.
2. Agent-scoped memory under `agent_home` records durable facts about the
   agent itself that are not tied to any workspace.
3. Episode, brief, and work-item evidence is automatically indexed with the
   relevant `workspace_id`.
4. Workspace profile and workspace-scoped generated memory summarize and route
   knowledge for a shared workspace.

This RFC defines the high-level boundaries. It intentionally does not define
the final storage schema, exact file names for all memory artifacts, the search
ranking algorithm, or the complete consolidation pipeline.

## Context

Holon already has three relevant accepted or draft contracts:

- `agent_home` is the agent identity root.
- `workspace_id` and `workspace_anchor` are stable workspace identity, while
  `execution_root` and `cwd` are projections used for tools.
- long-lived context memory already captures working memory and episode memory
  from durable runtime evidence.

The missing layer is the relationship between these pieces:

- what belongs to the agent rather than the workspace?
- what should be automatically loaded?
- what should be searchable but not always injected?
- what should be shared across agents through a workspace?
- how should Holon avoid making a session transcript the memory boundary?

## Goals

- Separate agent identity memory from workspace memory.
- Keep `agent_home/AGENTS.md` as the primary loaded agent guidance file.
- Make workspace identity the durable key for project-scoped recall.
- Index runtime evidence without requiring every message to be re-analyzed by
  a model.
- Support multi-agent use of the same workspace without requiring agents to
  directly edit shared memory files.
- Provide a clear place for workspace profile and generated workspace memory
  without making either one the sole source of truth.

## Non-Goals

- This RFC does not specify a `MemoryWrite` or `Remember` tool contract.
- This RFC does not require LLM-based extraction for every turn or every
  message.
- This RFC does not define vector search or embedding provider configuration.
- This RFC does not define a complete conflict-resolution algorithm for
  generated memory.
- This RFC does not replace the existing long-lived context memory RFC.

## Core Principles

### Memory is scoped before it is searched

Every durable memory item should have an explicit scope:

- agent scope
- workspace scope
- work-item or episode scope
- global or operator scope, if added later

Search may cross scopes, but storage and provenance should not be ambiguous.
The active workspace should be the default search scope for workspace-related
tasks, while agent-scoped memory should remain available across workspaces.

### Instructions are not the same as memory

Holon should treat instructions, memory, and evidence as different kinds of
state:

- instructions guide behavior and may be loaded into prompt context;
- memory summarizes durable facts, preferences, and lessons;
- evidence records what happened and remains available for audit and search.

An item can move between these layers over time, but the runtime should not
collapse them into one file or one prompt blob.

### Workspace identity comes from the runtime

Workspace-scoped memory must be keyed by `workspace_id`, not by a transient
shell cwd, worktree path, or session id.

Managed worktrees and isolated execution roots are projections of a workspace.
They must not create separate workspace memory identities unless the runtime
has intentionally attached a different workspace.

### Generated projections are not the source of truth

Workspace profiles, summaries, and generated Markdown memory are projections.
They should be rebuildable from stronger sources:

- workspace registry entries;
- loaded instruction files;
- repo manifests and docs;
- runtime ledger records;
- briefs, work items, and episodes;
- explicit operator or agent notes.

## Memory Surfaces

### 1. Agent identity memory: `agent_home/AGENTS.md`

`agent_home/AGENTS.md` is the loaded durable guidance for one agent.

It should carry:

- role definition;
- responsibilities;
- collaboration style;
- persistent operating habits;
- agent-specific constraints and expectations;
- agent-specific skill usage expectations.

This file is automatically loaded through the instruction-loading contract. It
is the right place for memory that must shape the agent's behavior every time
the agent runs.

It is not the right place for:

- current task progress;
- workspace-specific project facts;
- raw history;
- large experience logs;
- details that should only be recalled on demand.

### 2. Agent-scoped Markdown memory under `agent_home`

An agent may also need memory about itself that is not appropriate for
`AGENTS.md`.

Examples:

- personal long-term notes that are useful across workspaces but should not be
  loaded every turn;
- recurring mistakes or lessons that do not belong to one project;
- durable preferences about how this agent manages its own workflow;
- links to agent-local skills or runbooks;
- summaries of capabilities the agent has learned to use.

An agent may also need durable memory about the operator it works with:

- stable collaboration preferences;
- preferred level of autonomy;
- recurring delivery expectations;
- communication style;
- cross-workspace habits that should not be duplicated into every workspace
  profile.

This memory should live under `agent_home` and be treated as agent-scoped. The
key principle is that this memory belongs to the relationship between one
agent identity and its operator, not to any single workspace.

Phase 1 should use two Markdown files:

```text
agent_home/
  memory/
    self.md
    operator.md
```

`memory/self.md` is for the agent's own durable self model: role lessons,
cross-workspace workflow habits, recurring mistakes, capability notes, and
agent-local operating preferences that are too detailed for `AGENTS.md`.

`memory/operator.md` is for the agent's durable model of the operator: stable
preferences, collaboration patterns, autonomy expectations, communication
style, and cross-workspace rules that should influence future work but are not
mandatory enough to live in `AGENTS.md`.

Phase 1 should not split Markdown memory by date or allow arbitrary
agent-created topic files by default. If these files become too large, Holon
can later introduce `memory/topics/*.md` as a controlled expansion.

Only compact, high-priority slices should be loaded by default. The rest should
be searchable and retrievable on demand.

### 3. Workspace-indexed runtime evidence

Holon should automatically index runtime evidence produced during normal work:

- work items;
- work plans;
- briefs;
- context episodes;
- selected user messages;
- selected tool observations;
- verification evidence;
- result closure records.

Each indexed item should carry workspace metadata whenever the runtime can
determine it:

- `workspace_id`;
- `workspace_anchor`;
- `execution_root_id`, when relevant;
- `cwd`;
- work item id;
- episode id;
- timestamp;
- touched paths or artifacts, when known.

This index should use sanitized projections, not raw prompt text. It should
exclude system prompts, developer instructions, tool schemas, secrets, and long
raw tool outputs.

This layer should exist before any LLM-based memory consolidation. It covers
most recall needs cheaply because Holon already records work items, briefs, and
episodes as durable runtime state.

### 4. Workspace profile and workspace-generated memory

A workspace profile is the short, structured summary of a workspace.

It should answer:

- what workspace is this?
- what aliases and repo identity does it have?
- where are the instruction roots?
- what languages, frameworks, manifests, and docs matter?
- which commands are commonly used?
- what high-level conventions or gotchas are known?
- which memory or episode refs should be searched for deeper context?

Workspace-generated memory is a richer, retrievable layer around the profile.
It may contain:

- project-specific lessons;
- repeated failure modes;
- verified workflows;
- workspace-specific user preferences;
- durable decisions that are not already better represented in repo docs;
- pointers to relevant episodes and evidence.

The profile should stay small enough to support routing and prompt projection.
The generated memory may be larger, but should be retrieved on demand rather
than injected wholesale.

## Loading And Recall

### Loaded by default

The default prompt projection should load:

- runtime/base instructions;
- `agent_home/AGENTS.md`;
- workspace-scoped `AGENTS.md` for the active workspace;
- selected working memory and hot turn context;
- compact workspace profile information when relevant and within budget.

The runtime should not load all agent memory, all workspace generated memory,
or all historical episodes by default.

### Retrieved on demand

The following should be searchable and retrievable:

- agent-scoped self memory;
- workspace-generated memory;
- episode and brief index entries;
- work-item history;
- selected ledger projections;
- longer workspace memory notes.

The agent-facing recall surface should prefer one search tool plus one exact
expansion tool. Search applies the active workspace as the default scope and
returns provenance; exact expansion fetches one known source by `source_ref`.

Phase 1 exposes this as `MemorySearch` and `MemoryGet`. `MemorySearch` returns
enough provenance for follow-up (`kind`, `source_ref`, scope, workspace, source
path, title, snippet, score, timestamp, and metadata). `MemoryGet(source_ref,
max_chars?)` returns the same provenance plus bounded exact source text and a
`truncated` flag.

## Indexing Model

The first implementation should be allowed to use inexpensive indexing:

- exact keyword search;
- path and identifier search;
- FTS over sanitized projections;
- workspace-scoped filters and boosts.

The phase-1 implementation uses a rebuildable SQLite FTS5 index under
`agent_home/.holon/indexes/memory.sqlite3`. The index stores Holon-owned memory
records and curated projections only:

- `agent_home/memory/self.md`;
- `agent_home/memory/operator.md`;
- workspace profile records;
- briefs;
- context episodes;
- work items.

The derived SQLite index stores both original source text and indexed text.
Search uses the indexed projection, including CJK bigram expansion. `MemoryGet`
uses the original source body, so exact expansion does not leak tokenizer helper
terms into the agent context.

Workspace scoping is source-record owned. Runtime records that can become
workspace evidence, including briefs, context episodes, and work items, persist
their `workspace_id` at write time. Indexing reads that field directly rather
than inferring scope from `workspace_entered` or `workspace_used` events.

Normal workspace Markdown such as `README.md`, `docs/`, research notes, issue
drafts, and arbitrary `*.md` files are not indexed by `MemorySearch`.

SQLite's default tokenizer is not enough for Chinese and mixed CJK/Latin text,
so indexed text and query text are expanded with bounded CJK bigrams before FTS
matching.

The SQLite file is derived state. Runtime writes to indexed ledgers mark the
index dirty, successful controlled file writes repair known memory Markdown
paths when they are touched, and `MemorySearch` performs bounded stale repair
before querying. If the index is missing or dirty, it is rebuilt from the
source-of-truth records.

LLM extraction should not be required for every message. If Holon adds
LLM-based extraction later, it should be boundary-triggered:

- work item completion;
- episode finalization;
- wait boundary;
- compaction boundary;
- explicit operator request to remember something;
- workspace profile refresh.

This keeps memory costs bounded and avoids turning background memory into a
hidden second full conversation pass.

## Multi-Agent Workspace Sharing

Workspaces are shared host-owned resources. A workspace profile or
workspace-generated memory should not be directly edited by multiple agents at
the same time.

The preferred rule is:

- many agents may produce evidence and candidate memory facts;
- one workspace-scoped consolidation path materializes profile and memory
  projections;
- generated files are rebuildable from underlying evidence;
- conflict handling belongs to consolidation, not ordinary file editing.

This RFC does not define the consolidation lock or job schema, but the design
should preserve the ability to have one writer per `workspace_id` while
allowing different workspaces to consolidate independently.

## Relationship To Markdown

Markdown memory remains useful because it is readable and editable. It should
not become Holon's only memory source.

Recommended posture:

- `agent_home/AGENTS.md` is loaded guidance, not a general memory dump.
- `agent_home/memory/self.md` and `agent_home/memory/operator.md` are the
  phase-1 Markdown memory files. They should be searched and selectively
  loaded rather than injected wholesale.
- workspace-generated memory may be materialized as Markdown, but should be
  treated as a projection over evidence.
- current task state belongs in work items, briefs, and episodes, not in
  Markdown memory.

If a memory becomes a mandatory workspace rule, it should be promoted to
workspace instructions or repo documentation rather than staying only in a
generated memory note.

## Coverage

This model covers most expected memory needs:

- agent identity and behavior: `agent_home/AGENTS.md`;
- agent-local self knowledge: `agent_home/memory/self.md`;
- operator collaboration memory: `agent_home/memory/operator.md`;
- current and past task evidence: work items, briefs, episodes, and ledger
  projections indexed by workspace;
- project-specific recall: workspace profile plus workspace-generated memory;
- cross-agent workspace reuse: shared `workspace_id`-scoped memory and profile.

The model intentionally leaves advanced features for later:

- explicit memory write tools;
- semantic vector search;
- automatic long-term promotion;
- conflict-aware workspace memory editing;
- retention, decay, and forgetting policy.

## Open Questions

- Should workspace-generated memory be stored only in runtime state, only as
  Markdown, or both?
- When should Holon promote `memory/self.md` or `memory/operator.md` content
  into future `memory/topics/*.md` files?
- What is the minimum `MemorySearch` result shape needed for agents to cite
  retrieved memory safely?
- Should an explicit `Remember` tool be added in the first implementation, or
  only after indexing and search are stable?
- What token and job budgets should govern any future LLM-based extraction?

## Relationship To Other RFCs

- `agent-initialization-and-template.md` defines `agent_home` and
  `agent_home/AGENTS.md`.
- `instruction-loading.md` defines how agent and workspace instructions load.
- `workspace-binding-and-execution-roots.md` defines workspace identity and
  execution roots.
- `long-lived-context-memory.md` defines working memory and episode memory.
- This RFC defines how those pieces compose into agent-scoped and
  workspace-scoped memory.
