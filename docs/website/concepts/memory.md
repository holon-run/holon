---
title: Memory system
summary: How Holon's memory layers preserve continuity across turns — working memory, episodes, durable ledger, and indexed search.
order: 15
---

# Memory System

Holon is designed for long-lived agents. Memory preserves what matters across
turns without replaying the entire conversation history. The runtime derives
memory from durable evidence rather than relying on free-form model summaries.

## Memory Layers

Holon's context memory has four layers, each with a distinct role:

```
Durable Ledger  ──── append-only audit trail (messages, briefs, tool calls, tasks)
     │
     ▼
Working Memory  ──── compact current-state snapshot rebuilt after each turn
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
- Work item state changes
- Working memory deltas
- Episode records

The ledger is **not prompt-bounded**. It grows indefinitely as the audit
trail. The model-visible projection is a compressed selection, not a full
replay.

### Working Memory

Working memory is the compact current-state snapshot that answers:

- What work is active right now?
- What is the delivery target?
- Which constraints and scope limits apply?
- What follow-ups remain?
- What is the agent waiting on?

It is derived deterministically from runtime state and rebuilt after each turn
reaches closure. When the snapshot changes, the runtime appends a
**working memory delta** to the ledger.

Working memory is **not free-form summary**. It is a structured projection of
the runtime's own records — current work item, active todo list, pending
follow-ups, and waiting conditions.

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
not rendered in full by default.

### Context Assembly

Each turn assembles a prompt from budgeted memory sections:

- Hot turn context (current input, continuation, recent events)
- Turn-based context projection (linked turns, result briefs, task results,
  and work item transitions)
- Current work item and plan
- Relevant episode memory
- Working memory snapshot
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

## Memory and Work Items

Memory is tightly coupled to work items:

- **Current work item** anchors the working memory snapshot — objective,
  plan, todo list, and blocked status.
- **Episode records** are scoped to work items. When a work item completes,
  its accumulated evidence becomes an archived episode.
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
