---
title: Memory system
summary: How Holon's memory layers preserve continuity across turns — work items, work refs, episodes, durable ledger, and indexed search.
order: 15
---

# Memory System

Holon is designed for long-lived agents. Memory preserves what matters across
turns without replaying the entire conversation history. The runtime derives
memory from durable evidence rather than relying on free-form model summaries.

## Memory Indexing

Holon indexes memory asynchronously at startup. The index build runs in the
background and does **not block daemon startup**. New events continue to be
indexed as they are written to the durable ledger.

During the initial index build, search results may be incomplete. The
runtime prioritizes newly written events so recent memory is always
findable, even while the background build catches up on historical
records.

## Memory Layers

Holon's context memory has four layers, each with a distinct role:

```
Durable Ledger  ──── append-only audit trail (messages, briefs, tool calls, tasks)
     │
     ▼
Current Work Context ─ current WorkItem, todo state, waits, and refs
     │
     ▼
Episode Memory  ──── archived records of completed work chunks
     │
     ▼
Context Assembly ──── budgeted prompt sections selected from all layers
```

### Durable Ledger

The append-only source of truth. Every runtime event is recorded:

- Messages and briefs
- Tool executions and command results
- Task lifecycle transitions
- Work item state changes and current work refs
- Episode records

The ledger is **not prompt-bounded**. It grows indefinitely as the audit
trail. The model-visible projection is a compressed selection, not a full
replay.

### Current Work Context

Current work context is the compact runtime-owned projection that answers:

- What work is active right now?
- What is the current objective and plan?
- Which todo items, waits, and blockers matter?
- Which files, tool outputs, issues, PRs, tasks, or memories should remain easy
  to reopen?

The prompt-facing authority is the current `WorkItemRecord` plus its
runtime-derived `work_refs`. Work refs are extracted at turn closure from
trusted runtime evidence such as current input source refs and tool execution
records. The model does not author them directly.

Current work context is **not free-form summary**. It is a structured projection
of the runtime's own records: current WorkItem state, active todo list,
blockers, waiting conditions, and refs back to retrievable evidence.

### Episode Memory

Episode memory archives completed work. While work is in progress, an active
episode builder accumulates:

- Active work item ID and delivery target
- Work summary and scope
- Touched files
- Verification evidence
- Decisions made
- Carry-forward follow-ups

When a meaningful boundary is reached (work item completed, task finished),
the runtime finalizes the builder into an immutable **episode record** and
stores it.

Archived episodes are selected into prompt context by relevance and budget,
not rendered in full by default. Default prompt assembly treats episodes as a
mid-term archive: it excludes episodes that overlap the `recent_turns` window
so recent turn evidence is not duplicated by a summary of the same turns.

### Context Assembly

Each turn assembles a prompt from budgeted memory sections:

- Hot turn context (current input, continuation, recent events)
- Turn-based context projection (linked turns, result briefs, task results,
  and work item transitions)
- Current work item and plan
- Current work refs
- Relevant episode memory
- Execution environment projection

This assembly keeps prompt size bounded while preserving continuity.
Slow-changing memory sections keep provider cache identity stable.
For a user-facing walkthrough of how these sections fit together, see
[Context Continuity](/concepts/context-continuity.md).

## Memory vs Agent Home Files

Memory and agent home files serve different purposes:

| Aspect | Runtime Memory | Agent Home Files |
|--------|---------------|-----------------|
| **What it stores** | Current state, episodes, evidence | Role contract, notes, references |
| **Who writes it** | Runtime (automatic) | Agent or operator (manual) |
| **Durability** | Append-only ledger + snapshots | Persistent files |
| **Search** | Indexed via `MemorySearch` | Ordinary file read |
| **Loaded** | Budgeted prompt assembly | `AGENTS.md` always loaded |

`agent_home/AGENTS.md` is loaded guidance — the agent's long-lived role
contract. Runtime memory is automatically derived evidence. They coexist
without overlapping.

## MemorySearch and MemoryGet

Holon exposes two memory tools for indexed retrieval:

- **`MemorySearch`** — Search across memory sources (agent memory markdown, runtime evidence) by query. Returns ranked results with opaque `source_ref` values.
- **`MemoryGet`** — Fetch exact memory content by `source_ref`. Used to retrieve a specific record identified by search.

These tools let the agent pull relevant past context on demand without
rendering every archived episode into every prompt.

## Agent Memory Auto-Load

Holon automatically injects a compact slice of the agent's curated memory
files into every turn's system prompt. This gives the agent persistent
self-knowledge and operator preferences without manual recall or search.

Two files participate in auto-load:

| File | Purpose | Who writes it |
|------|---------|---------------|
| `agent_home/memory/operator.md` | Operator preferences, standing instructions | Operator |
| `agent_home/memory/self.md` | Agent self-knowledge, role facts | Agent |

At turn assembly, each file is read and a **compact slice** is injected under
a fixed per-file character budget (default 1500 characters). If the file
exceeds the budget, the injected slice is truncated and the agent receives a
note that the remainder is retrievable via `MemoryGet`. If the file is empty,
the agent receives a note that curated content is not yet present.

The auto-loaded sections appear in the prompt as:

- **`agent_memory_operator`** — Curated operator memory, loaded with
  `Stability::AgentScoped` (changes only when the operator edits the file).
- **`agent_memory_self`** — Curated self memory, loaded with
  `Stability::AgentScoped` (changes only when the agent edits the file).

These sections sit between `AGENTS.md` guidance and the workspace scope in
the prompt hierarchy. They carry lower authority than workspace or turn-scoped
instructions but provide persistent facts that survive context compaction.

### When to use each file

- **`operator.md`** — Store cross-agent operator preferences: preferred
  language, naming conventions, tool defaults, communication style. These
  apply regardless of which agent is running.
- **`self.md`** — Store agent-specific durable facts: the agent's role,
  standing responsibilities, past decisions worth remembering, recurring
  workflow notes.

## Notes Catalog

In addition to curated memory files, Holon can inject a metadata catalog of
the agent's `notes/` directory into the prompt. The notes catalog acts as a
bounded reference index, not as instruction content.

The catalog is rendered from each Markdown file in `agent_home/notes/` and
includes:

- **Title** — extracted from frontmatter, the first heading, or the filename.
- **Summary** — extracted from frontmatter or the first paragraph excerpt.
- **Tags** — extracted from frontmatter (lower-cased, deduped).

The catalog is bounded: at most 20 entries and 2000 total characters. Note
bodies are **never** injected — the catalog is a metadata index only. The
agent can retrieve full note content by reading the referenced file.

Notes are treated as reference material, not as instructions. They do not
override operator input, AGENTS.md guidance, or the current WorkItem objective.

## Memory and Work Items

Memory is tightly coupled to work items:

- **Current work item** anchors the live continuity state: objective, plan,
  todo list, blocked status, and current work refs.
- **Episode records** are scoped to work items. When a work item completes,
  its accumulated evidence becomes an archived episode.
- If a current work item and an episode both match the prompt, the work item
  remains authoritative for current objective, plan, todo, and wait state; the
  episode is supporting historical evidence.
- **MemorySearch** indexes across completed episodes, making past work
  findable by content rather than by timestamp alone.

This means Holon can remember what it did for a previous issue without
re-reading the entire transcript of that work.

## Memory Boundaries

Holon separates memory by identity scope:

- **Agent-scoped memory** — Working memory, episodes, and search index belong
  to a specific agent.
- **Workspace-scoped memory** — Episode records are tagged with the
  `workspace_id` where the work happened.
- **Curated durable memory** — `agent_home/memory/self.md` and
  `agent_home/memory/operator.md` are manually curated Markdown files for
  agent-specific facts and operator preferences.

These boundaries prevent session transcripts from becoming the only memory
surface and let shared workspaces accumulate knowledge across multiple agents.

## See Also

- [Context Continuity](/concepts/context-continuity.md) — How Holon keeps context coherent without replaying the entire transcript
- [Runtime Model](/concepts/runtime-model.md) — Agents, work items, and the execution loop
- [Documentation Layers](/concepts/documentation-layers.md) — How memory fits into Holon's documentation architecture
- [Agent Templates](/guides/agent-templates.md) — How templates initialize agent role contracts
- RFC: [Long-Lived Context Memory](https://github.com/holon-run/holon/blob/main/docs/rfcs/long-lived-context-memory.md)
- RFC: [Agent and Workspace Memory](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-and-workspace-memory.md)
