---
title: Runtime model
summary: Agents, tasks, work items, workspaces, and the execution loop that make up Holon's runtime.
order: 10
---

# Runtime Model

Holon treats agent execution as a runtime system. A single model turn is
important, but it's not the whole picture. The runtime tracks durable identity,
active work, supervised tasks, wake conditions, and final delivery as separate
concerns — each with its own lifecycle.

## Core Concepts

### Agents

An agent is an **addressable runtime actor** with:

- **Identity** — A unique `agent_id` used for addressing messages and
  inspecting state
- **Lifecycle** — Created, active, sleeping, and eventually terminated
- **Workspace** — A project root where the agent reads and mutates files
- **Guidance** — Loaded from `AGENTS.md` (project and agent-level)
- **Local state** — Agent-specific memory, pending follow-ups, and work item
  focus

Agent profiles:

- **Public (self-owned):** Standalone identity, self-managed lifecycle. Used
  for long-running, addressable agents.
- **Private (child):** Parent-supervised via a task handle. Used for delegated
  subtasks and parallel work.

### Work Items

A work item is a **durable objective record** that outlives individual model
turns. It contains:

- **Objective** — The short goal statement (e.g. "Fix build warnings in src/")
- **Plan** — Durable multi-step plan in prose
- **Plan status** — `draft`, `ready`, or `needs_input`
- **Todo list** — Discrete progress checklist items
- **Blocked by** — Specific blocker description when progress stalls

Work items let Holon resume work across turns, inspect progress, or hand off
incomplete work to another agent. They are not chat history — they're a
**project management primitive** built into the runtime.

Work item lifecycle:

```
[Created] -> [Draft plan] -> [Ready] -> [In progress] -> [Completed]
                ^                            |
                +--- [Needs input] <-- [Blocked]

When a work item is completed, the runtime promotes the agent's completion
text as a **completion report**. The pattern is:

1. Agent writes the operator-facing summary as assistant text
2. Agent calls `CompleteWorkItem` in the same turn
3. Runtime promotes the preceding text as the canonical completion report

Completion reports are stored as part of the work item record. They are
visible through `GetWorkItem` and `ListWorkItems`, and indexed by
`MemorySearch` for future recall. This makes it possible to ask "what did we
conclude on that issue?" without re-reading the full transcript.

Completion reports replace free-form manual summaries. They are tied to the
work item lifecycle, not to individual model turns.
```

### Tasks

A task is a **supervised execution handle**. Tasks include:

- **Command tasks** — Shell commands, builds, tests, scripts
- **Child agent tasks** — Delegated agents spawned via `SpawnAgent`

Task lifecycle is independent of the agent's user-facing answer. You can:

- Inspect status (`TaskStatus`)
- Read bounded output (`TaskOutput`)
- Send continuation input (`TaskInput`)
- Stop explicitly (`TaskStop`)

### Queues and Wakeups

Holon's scheduling primitives manage when an agent acts and when it rests:

- **Enqueue** — Schedule a follow-up message for this agent. Priorities:
  `interject`, `next`, `normal`, `background`.
- **Sleep** — Agent goes idle when no immediate work remains.
- **Wake** — External trigger or queued message reactivates the agent.

These state transitions are visible — integrations don't need to infer hidden
background behavior.

### External Triggers

External triggers let an agent wait for events from outside the runtime:

```text
Agent waits ──► External trigger created ──► Event arrives ──► Agent wakes
```

Use `CreateExternalTrigger` to register a waiting intent and
`CancelExternalTrigger` to revoke it when no longer needed.

**Delivery modes:**

| Mode | Behavior |
|------|----------|
| `wake_hint` | Wakes the agent so it can inspect external state (e.g., check a CI run). The trigger payload is not enqueued as a message. |
| `enqueue_message` | Wakes the agent **and** delivers the trigger payload as a message in the agent's queue. |

Choose `wake_hint` when the external system already has a query API (GitHub
API, CI status endpoints). Choose `enqueue_message` when the callback payload
itself contains the actionable information.

**Scope:**

| Scope | Lifetime |
|-------|----------|
| `work_item` | Tied to the current work item. Automatically cancelled when the work item completes. |
| `agent` | Survives across work items. Use for long-running integration entry points. |

Common integration patterns:

- **Waiting for CI** — `CreateExternalTrigger` with `source="github"`,
  `scope=work_item`, `delivery_mode=wake_hint`. Agent wakes when CI completes
  and checks the run status.
- **Webhook callbacks** — `delivery_mode=enqueue_message` so the webhook body
  enters the agent queue with provenance preserved.

Stale triggers waste resources. Cancel work-item-scoped triggers when the
current task, tracked target, or waiting condition changes.

### Delivery

Holon separates **internal execution traces** from **user-facing delivery**:

- **Briefs** — Condensed context summaries for model consumption
- **Final answer** — The useful result shown to the operator
- **Task output** — Command stdout/stderr, available through task inspection
- **Transcripts** — Full turn history for debugging

### Workspaces

Every agent has exactly one active workspace. Workspaces define:

- **Instruction root** — Where `AGENTS.md` and policy files are resolved
- **Execution root** — Default working directory for commands
- **ApplyPatch target** — Where file mutations land

Workspaces can be attached, detached, or isolated for safe experimentation.

## The Operating Loop

Each agent turn follows this pattern:

1. **Ingress** arrives with `origin`, `trust`, and `priority` metadata.
2. **Anchor** — Non-trivial work gets a work item with a stable objective.
3. **Load context** — Agent reads only what's needed for the current decision.
4. **Mutate** — Changes happen through explicit workspace tools (`ApplyPatch`,
   `ExecCommand`).
5. **Verify** — Run real project checks (`cargo test`, `cargo check`) when
   available.
6. **Deliver** — Concise user-facing result, then sleep or enqueue follow-up.

## See Also

- [Memory System](/concepts/memory.md) — How Holon preserves continuity across turns
- [Trust Boundaries](/concepts/trust-boundaries.md) — How Holon classifies and
  enforces trust
- [CLI Reference](/reference/cli.md) — All CLI commands
- [Integration Guide](/guides/integration.md) — HTTP control plane API
- [Getting Started](/getting-started/first-agent.md) — Hands-on tutorial
