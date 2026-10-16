---
title: Runtime model
summary: How the daemon, clients, agents, work items, tasks, and workspaces fit together.
order: 10
---

# Runtime model

Holon is a headless runtime for agents that can work beyond a single chat
interaction. The daemon owns execution and durable state. Clients such as the
CLI and Web GUI submit work and display results; they do not need to hold the
entire execution in a chat window.

An agent activation may involve several model calls and tool operations.
The agent's identity and tracked objectives can outlive that activation.
A one-shot run is useful for a bounded job; a persistent agent is useful when
you want to return to the same worker and its accumulated context.

```text
Clients and Local Apps ── requests / results ── Daemon
                                                │
Template ── initial role and skills ────────── Agent
                                                ├── guidance and memory
                                                ├── models and provider routes
                                                ├── skills → tool operations
                                                ├── workspace → execution root
                                                └── WorkItem → tasks and waits
                                                               → completion report
```

The last branch is optional: a short question or current-turn task need not
create a WorkItem.

[Trust boundaries](./trust-boundaries.md) govern how inputs are interpreted.
[Execution boundaries](./security-and-execution-boundaries.md) describe what
operations can reach. [Context continuity](./context-continuity.md) connects
the current activation to saved work and evidence.

## Core concepts

### Agents

An [agent](./agents.md) is an addressable worker with its own guidance, memory,
and work state. An [Agent Template](./agent-templates.md) can initialize its
role; [skills](./skills-and-tools.md) provide workflows it can load as needed.
[Models and provider routes](./models-and-providers.md) supply model capabilities
for the agent's calls. These choices do not grant permissions.

### Work items

A [WorkItem](./work-items-and-tasks.md) preserves a goal, plan, progress, and
acceptance criteria when the work needs tracking or continuity. It remains
open until the objective is explicitly completed. Current focus, readiness,
and completion are separate properties.

### Tasks

A task tracks a particular execution, such as a background command or delegated
invocation. Its status and output provide evidence for the objective. A test
command finishing does not automatically complete the WorkItem that requested
it.

### Queues and wakeups

Messages and events let the runtime schedule an agent. A recorded wait tells
it what continuation the agent expects. Ending a reply or writing a reminder
in a plan does not register a wait. See
[Events, waits, and wakeups](./events-and-waits.md).

Task lifecycle is independent of the agent's user-facing answer. You can:
- Inspect status (`TaskStatus`)
- Read runtime-confirmed command files in bounded ranges, or observe agent
  progress with bounded `holon tail --agent <id> --include-progress`
- Wait for completion and result re-entry (`WaitFor(wake=task_result,
  resource=<task_id>, delivery=...)`); observation/EOF is not completion
- Send continuation input (`TaskInput`)
- Stop explicitly (`TaskStop`)

### External triggers

External ingress can signal that outside state changed or deliver a message.
A wake hint does not pass its payload to the model; the agent checks the
source after waking. Receiving an event neither grants authority nor proves
that the outside work succeeded.

### Delivery

The operator receives a concise result, with deliverables and verification
status. Task output and transcripts remain separate evidence surfaces for
inspection. A WorkItem's completion report records the outcome of that
objective. A [Local App](./local-apps.md) can provide a focused interface for
requesting and inspecting work. See [Results and artifacts](./results-and-artifacts.md).

### Workspaces

The active [workspace](./workspaces.md) supplies project context and tool
location defaults. An execution root can be a canonical checkout or a linked
worktree. These bindings organize work; they do not create a host filesystem
sandbox.

## The operating loop

Suppose you ask an agent to fix a failing build and wait for CI. It inspects
the project, records the objective if the work will span turns, makes the
change, and runs local checks. It then records a wait for the CI run. After a
notification, it checks that run and the remaining acceptance criteria before
reporting the outcome.

You can disconnect your client while the daemon continues this work. Restarting
the daemon or interrupting a command is a different situation: durable work
records help recovery, but do not promise uninterrupted processes. See
[Context continuity](./context-continuity.md).

## See also

- [Create your first agent](../getting-started/first-agent.md)
- [Track long-lived work](../guides/run-long-lived-task.md)
- [Memory](./memory.md)
- [Trust boundaries](./trust-boundaries.md)
- [CLI reference](../reference/cli.md)
