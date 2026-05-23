---
title: Concepts
summary: The mental model behind Holon — four simple objects that make agents durable.
order: 20
---

# Concepts

Holon is easiest to understand as a runtime, not a chat wrapper. The runtime
centers on four simple objects that work together:

## The mental model in four objects

**Agents** are the actors. Each agent has a durable home directory, its own
guidance (`AGENTS.md`), local memory, and a work queue. You address an agent by
its ID, and it persists across turns — it can sleep, wake, and continue work
without losing state.

**Work items** are the objectives. They capture *what* the agent is trying to
achieve, with a durable plan, a todo checklist, and a completion goal. Work
items survive across turns and model calls, so an agent can resume after hours
or days without losing progress.

**Tasks** are the execution. Every command, every child agent delegation, every
background operation is wrapped in a task handle. You can inspect task status,
read output, send input, or stop a task — the runtime tracks it all.

**Trust boundaries** are the provenance. Operator input, external webhooks,
child agent output, and web-sourced content each carry an origin and a trust
level. Holon never flattens them into one undifferentiated prompt stream.

```
Agent (who)
  └── Work items (what they're doing)
        └── Tasks (how they do it)
              └── Trust classification (where input came from)
```

## Why this matters

Most agent tools work like chat sessions: a prompt goes in, a response comes
out, and the state disappears. Holon is different because:

- You can walk away and the agent keeps working.
- You can inspect *what* the agent is doing without reading the entire
  transcript.
- You can see *where* each input came from and how much to trust it.
- You can delegate work to child agents and supervise their progress.

## Deeper reading

The **memory system** page explains how Holon preserves continuity across
turns through working memory, episode archives, and indexed search.

The **runtime model** page expands the four-object mental model into precise
lifecycle vocabulary: agent profiles, work-item states, task kinds, queue
semantics, triggers, and workspace isolation.

The **trust boundaries** page is product-oriented: it explains why origin and
trust matter for real integrations, not just as security theory.

The **documentation layers** page explains how Holon separates product docs,
current-contract reference, and maintainer design records — so you know which
documents are canonical for which purpose.

For the canonical design contracts behind these concepts, see the repository
[RFCs](https://github.com/holon-run/holon/tree/main/docs/rfcs) and
[implementation
decisions](https://github.com/holon-run/holon/tree/main/docs/implementation-decisions/).
These are maintainer-facing documents; you do not need them to use Holon.

<!-- INDEX:START -->

- [Runtime model](./runtime-model.md)
  Agents, tasks, work items, workspaces, and the execution loop that make up Holon's runtime.
  <!-- mdorigin:index kind=article -->

- [Memory system](./memory.md)
  How Holon's memory layers preserve continuity across turns — working memory, episodes, durable ledger, and indexed search.
  <!-- mdorigin:index kind=article -->

- [Trust boundaries](./trust-boundaries.md)

- [Security and execution boundaries](./security-and-execution-boundaries.md)
  What Holon does and does not sandbox and how to run agents safely.
  <!-- mdorigin:index kind=article -->
  How Holon classifies origin, trust, and priority to keep long-lived agents secure.
  <!-- mdorigin:index kind=article -->

- [Documentation layers](./documentation-layers.md)
  How Holon separates user-facing docs, current-contract reference, and maintainer design records — and which layer to read for which question.
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
