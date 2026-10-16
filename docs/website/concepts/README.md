---
title: Concepts
summary: Understand agents, reusable roles, workspaces, durable work, and the boundaries you can rely on.
order: 20
---

# Concepts

These pages explain what Holon's objects mean, how they relate, and which
behaviors you can rely on. Start with the runtime model, then read the pages
that support your work.

The first group covers agents and their setup: guidance, templates, skills,
and workspaces. The next follows continuing work through objectives, tasks,
events, collaboration, memory, and context. The remaining pages explain model
selection, Local Apps, delivered results, and trust and access boundaries.

For a first successful request, use [Getting started](../getting-started/).
For steps toward a particular goal, use [Guides](../guides/). Exact options
and schemas belong in [Reference](../reference/); internal state machines
belong in [Runtime specs](../spec/).

<!-- INDEX:START -->

- [Runtime model](./runtime-model.md)
  How the daemon, clients, agents, work items, tasks, and workspaces fit together.
  <!-- mdorigin:index kind=article -->

- [Agents, roles, and guidance](./agents.md)
  Agent identity, Agent Home, and the instructions that define an agent's responsibilities.
  <!-- mdorigin:index kind=article -->

- [Agent templates](./agent-templates.md)
  Reusable starting points for an agent's role and skills, separate from the agent instance they create.
  <!-- mdorigin:index kind=article -->

- [Skills and tools](./skills-and-tools.md)
  How reusable workflows relate to tool capabilities, skill management, and loaded agent guidance.
  <!-- mdorigin:index kind=article -->

- [Workspaces and working directories](./workspaces.md)
  Agent Home, project workspaces, execution roots, and worktrees, with the limits of workspace isolation.
  <!-- mdorigin:index kind=article -->

- [Work items and tasks](./work-items-and-tasks.md)
  Durable objectives and supervised execution handles, including planning, waits, ownership, and completion.
  <!-- mdorigin:index kind=article -->

- [Events, waits, and wakeups](./events-and-waits.md)
  How messages, task results, timers, and external events let an agent pause and resume authorized work.
  <!-- mdorigin:index kind=article -->

- [Multi-agent collaboration](./multi-agent-collaboration.md)
  Peer agents, supervised children, and the responsibility for delegated results and cleanup.
  <!-- mdorigin:index kind=article -->

- [Memory](./memory.md)
  Runtime evidence, curated memory, and ordinary notes, with their loading, search, and authority limits.
  <!-- mdorigin:index kind=article -->

- [Context continuity](./context-continuity.md)
  How durable work state helps an agent resume without replaying all history, and what interruptions still require recovery.
  <!-- mdorigin:index kind=article -->

- [Models, providers, and routing](./models-and-providers.md)
  Model capabilities, executable provider routes, agent overrides, fallback, Fast mode, and advisory decisions.
  <!-- mdorigin:index kind=article -->

- [Local apps](./local-apps.md)
  Agent-hosted interfaces, their request and event flow, and the limits of shared-session access.
  <!-- mdorigin:index kind=article -->

- [Results, artifacts, and observable state](./results-and-artifacts.md)
  How to distinguish accepted work, execution progress, completion reports, task output, and delivered files.
  <!-- mdorigin:index kind=article -->

- [Trust boundaries](./trust-boundaries.md)
  How source, instruction authority, and scheduling priority differ across operator input, guidance, and external evidence.
  <!-- mdorigin:index kind=article -->

- [Security and execution boundaries](./security-and-execution-boundaries.md)
  Host-local execution, workspace limits, authenticated access, and capability-secret risks.
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
