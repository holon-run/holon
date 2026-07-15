---
title: Holon
summary: A local workbench for agents doing continuous work.
order: 1
---

# Holon

**Holon is a local workbench for agents doing continuous work.**

Holon itself is not an agent. It provides a local working environment for
multiple agents. Agents understand goals and drive execution; Holon treats work
as the core unit, preserving state, organizing context, recording waits and
wakes, so tasks that span sessions, commands, human confirmation, or external
events can resume at the right time and eventually deliver results back to the
operator.

## What Holon provides

| Capability | What it means |
|---|---|
| **Continuous agent workspace** | Each agent has its own continuous working context in Holon, instead of restarting with every terminal, request, or client connection. |
| **Work-first task model** | Holon organizes tasks, waits, execution progress, and final delivery as explicit Work, instead of leaving them scattered across conversations. |
| **Event-driven wait and wake** | Agents can wait for task results, external events, or operator input, then return to the corresponding work when the condition is satisfied. |
| **Explicit context and trust boundaries** | Holon distinguishes operator input, external events, tool results, and internal execution traces so information from different origins is not mixed together. |
| **Local-first execution environment** | Holon is built for local repositories, shell, worktrees, and development toolchains, letting agents execute tasks in the real working environment. |

> Keep agent work alive in your local workspace.

## Try Holon

Holon provides two interaction modes: **TUI** (terminal) and **Web GUI** (browser).

### Install

```bash
brew tap holon-run/tap && brew install holon
holon --help
```

Or download binaries from [GitHub Releases](https://github.com/holon-run/holon/releases/latest).

### Configure a provider

```bash
holon onboard
```

This walks through provider credential setup interactively. You can also
configure providers through the Web GUI **Settings** page after starting the
daemon. See [Configuration Reference](/reference/configuration) and
[Web GUI guide](/guides/web-gui) for more.

### Start the daemon

```bash
holon daemon start
```

### TUI (terminal)

```bash
holon tui
```

Select an agent and start working. Agents keep running after you disconnect.

### Web GUI (browser)

Open <http://localhost:7878>. Create an agent and work through a chat interface
with built-in file browser, task tracking, and more.

Holon automatically provides a default main agent. You can create more
specialized agents through the TUI or Web GUI. For more:
[Getting started](/getting-started/) · [TUI guide](/guides/tui) ·
[Web GUI guide](/guides/web-gui)

Holon supports providers such as Anthropic, OpenAI, DeepSeek, OpenRouter, Qwen,
GLM, Xiaomi, Kimi, and MiniMax. For advanced setup, see the
[configuration reference](/reference/configuration), and
[supported models](/reference/models).

## Core concepts

Holon breaks agent work into a few explicit runtime objects:

- **Agent** is a long-lived local identity with its own queue, state, history,
  and working context.
- **WorkItem** represents a continuously advanceable goal, including a plan,
  progress, blockers, wait conditions, and a completion report.
- **Task** represents supervised asynchronous execution, such as a command,
  background task, or child agent.
- **WaitFor / wake** lets an agent explicitly declare that it is waiting for a
  task result, external event, or operator input, and resume when the condition
  is satisfied.
- **Workspace / worktree** lets agents execute in local repositories and isolate
  coding tasks into managed worktrees.
- **Origin / brief** preserves input origin and trust information while keeping
  internal execution traces separate from operator-visible delivery.

Together, these concepts solve one problem: agent work should not depend on a
single chat or terminal connection. It should be observable, resumable,
waitable, delegable, and deliverable.

## Status and compatibility

The current recommended release is
[`v0.30.0`](https://github.com/holon-run/holon/releases/tag/v0.30.0).

`v0.15.0` is the baseline release where the Holon Rust runtime enters public
compatibility maintenance. Starting from this version, the project maintains
compatibility expectations for the CLI, daemon/API semantics, and local
persistent storage.

Holon is still under active development. The current focus remains the Rust
runtime: agent lifecycle, queues, WaitFor/wake, tasks, WorkItems, trust
boundaries, local workspaces, and structured delivery.

## Project boundaries

Holon focuses on runtime semantics: agent identity, work continuity, execution
state, local workspace projection, and operator-visible results.

Adjacent Holon Run projects cover other layers:

- **[AgentInbox](https://github.com/holon-run/agentinbox)** — source hosting,
  activation, and delivery
- **[UXC](https://github.com/holon-run/uxc)** — unified capability and tool
  access
- **[WebMCP Bridge](https://github.com/holon-run/webmcp-bridge)** — browser and
  web-app edge access

When used together, AgentInbox delivers external events to wake Holon; Holon
decides what those events mean inside the runtime.

## Which docs should I read?

- **I want to install and run Holon** → [Getting started](/getting-started/)
- **I want to understand the concepts** → [Concepts](/concepts/), especially
  [runtime model](/concepts/runtime-model) and
  [security and execution boundaries](/concepts/security-and-execution-boundaries)
- **I want to find a command or config key** → [Reference](/reference/)
- **I want to integrate Holon** → [Integration guide](/guides/integration)
- **I want to contribute to the runtime** →
  [Architecture overview](https://github.com/holon-run/holon/blob/main/docs/architecture-overview.md)
  and [RFCs](https://github.com/holon-run/holon/tree/main/docs/rfcs)

<!-- INDEX:START -->

- [Runtime specs](./spec/)
  <!-- mdorigin:index kind=directory -->

- [Getting started](./getting-started/)
  <!-- mdorigin:index kind=directory -->

- [Concepts](./concepts/)
  <!-- mdorigin:index kind=directory -->

- [Guides](./guides/)
  <!-- mdorigin:index kind=directory -->

- [Reference](./reference/)
  <!-- mdorigin:index kind=directory -->

<!-- INDEX:END -->
