---
title: Holon
summary: A local-first runtime that gives agents a durable home, explicit work queues, and clear trust boundaries.
order: 1
---

# Holon

**Holon gives every agent a durable home.**

Instead of starting each agent as a throwaway chat session, Holon runs agents
inside a local-first, headless, event-driven runtime. Agents keep state across
turns, manage queued work, supervise delegated tasks, and respect explicit trust
boundaries — all on your machine.

## Who Holon is for

Holon is built for three kinds of users right now:

**Agent runtime builders** who want a durable execution foundation — not a
prompt chain or a framework that conflates the model call with the agent's
lifecycle.

**Automation and integration developers** who need agents that run locally, wait
for external events, resume work, and produce structured output without a
browser tab open.

**Contributors** evaluating whether Holon's runtime model matches their
expectations for lifecycle, trust, and local-first design before investing
deeper.

## How Holon is different

Most agent tools flatten everything into a chat message. Holon preserves
structure:

- **Local-first** — the runtime runs on your machine, not a cloud session.
  Agents own durable homes, and you control persistence.
- **Headless and long-lived** — agents sleep, wake, queue work, and continue
  across turns without a UI needing to stay connected.
- **Explicit lifecycle** — work items, tasks, queues, triggers, and child agents
  are first-class runtime concepts, not hidden state in a prompt loop.
- **Trust-aware provenance** — operator input, external events, and delegated
  outputs each carry their origin and trust classification through the runtime
  instead of being merged into one undifferentiated stream.

Holon is designed for APIs, workers, CLIs, and integrations before UI shells.
The agent home is a real directory; the work queue is explicit; the runtime
contracts are visible.

## Try Holon

```bash
brew tap holon-run/tap
brew install holon
holon --help
```

This gets you from zero to a running Holon binary. To build from source or
contribute, see [Getting started](/getting-started/). Next, follow the
[getting started guide](/getting-started/) to create your first agent.

> **Holon is early-stage software.** The runtime model is stabilizing, but CLI
> shapes, config schemas, and provider surfaces may change. See the
> [reference pages](/reference/) for the current surface and the repository
> [RFC index](https://github.com/holon-run/holon/tree/main/docs/rfcs) for design direction.

## Which docs should I read?

- **I want to install and run Holon** → [Getting started](/getting-started/)
- **I want to understand the concepts** → [Concepts](/concepts/), especially
  [runtime model](/concepts/runtime-model) and
  [documentation layers](/concepts/documentation-layers)
- **I want to find a command or config key** → [Reference](/reference/)
- **I want to integrate Holon** → [Integration guide](/guides/integration)
- **I want to contribute to the runtime** →
  [Architecture overview](https://github.com/holon-run/holon/blob/main/docs/architecture-overview.md)
  and [RFCs](https://github.com/holon-run/holon/tree/main/docs/rfcs)

## Documentation map

- [Getting started](/getting-started/) — your first Holon agent run, from
  install to first interaction.
- [Concepts](/concepts/) — the mental model: agents, work items, tasks, queues,
- **[Security and execution boundaries](/concepts/security-and-execution-boundaries)** — what Holon guards and what you must guard.
  and trust boundaries.
- [Guides](/guides/) — task-oriented workflows for operating, integrating, and
  extending Holon.
- [Reference](/reference/) — current CLI, configuration, and control-plane
  surface documentation.
  experimental.
- [Specs](/spec/) — current implementation-facing runtime contracts for
  maintainers and contributors.

## For contributors

Holon's internal design material lives in the [repository `docs/`
directory](https://github.com/holon-run/holon/tree/main/docs): RFCs define
runtime contracts, [spec pages](/spec/) extract current verified contracts,
implementation decisions record architecture rationale, and archived notes
preserve historical context. These are maintainer-facing documents; you do not
need to read them to use Holon, but they are the canonical source when you
need to understand or change runtime behavior.

## About this site

This website is built from source Markdown with mdorigin. Every page is
available as both rendered HTML and raw Markdown, and `Accept: text/markdown`
requests return machine-readable content. See the
[documentation workflow guide](/guides/documentation-workflow) for build and
preview details.

## Markdown-native access

mdorigin keeps the site useful for both humans and agents:

- Browser routes render HTML.
- Explicit `.md` routes return the source Markdown.
- `Accept: text/markdown` requests can retrieve Markdown content directly.
- Build commands can generate search data and Cloudflare Worker assets.

Build commands are covered in the [documentation workflow
guide](/guides/documentation-workflow). The production `siteUrl` is
`https://holon.run`.

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
