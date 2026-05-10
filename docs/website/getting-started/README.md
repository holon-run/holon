---
title: Getting started
summary: Go from zero to your first Holon agent in under 15 minutes.
order: 10
---

# Getting started

Holon is currently built from source as a Rust binary. This section gives you
the shortest path from clone to a running agent, then shows where to branch
based on what you want to do next.

## New to Holon?

If this is your first time using Holon, follow our step-by-step tutorial:

- **[Create your first agent](first-agent.md)** - Build, start, connect with TUI, create an agent, and configure models in ~15 minutes

This hands-on guide covers:

- Building and verifying the Holon binary
- Starting the runtime in daemon mode
- Connecting with the Terminal UI
- Creating an agent and sending your first prompt
- Configuring models and providers

## Which runtime mode should I use?

Holon gives you three ways to interact with the runtime:

| Mode | Command | Best for |
|------|---------|----------|
| **One-shot** | `holon run "..."` | Quick single-turn tasks — no daemon needed |
| **Daemon + TUI** | `holon daemon start` + `holon tui` | Interactive agent sessions with state, queues, and workspaces |
| **Daemon + HTTP** | `holon daemon start` + HTTP client | Integrations, automation, control-plane consumers |

The [first agent tutorial](first-agent.md) uses daemon + TUI because it
gives you the full interactive experience. For one-shot runs, see the
[quick examples](/guides/quick-examples/).

## Evaluate or explore?

If you're already familiar with Holon or want to jump straight into specifics:

- **[Quick examples](/guides/quick-examples/)** — one-shot and common task patterns
- **[Concepts](/concepts/)** — the mental model before diving into internals
- **[CLI reference](/reference/cli.md)** — full command surface
- **[Troubleshooting](/guides/troubleshooting/)** — diagnose common setup issues

## Contribute or develop?

If you plan to modify or contribute to Holon itself:

- **[Local runtime guide](/guides/local-runtime/)** — conservative development workflow
- **[Documentation workflow](/guides/documentation-workflow/)** — how to build and preview this site
- **[Integration guide](/guides/integration/)** — wire Holon into external systems
- Repository `docs/` directory — RFCs, implementation decisions, and architecture notes

## Requirements

- Rust toolchain with Cargo (build from source; pre-built binaries not yet available)
- A model provider API key (Anthropic, OpenAI, or compatible)
- Node.js and mdorigin (only needed for building this documentation site)

## Repository orientation

- `src/` contains the Rust runtime implementation and executable entrypoints.
- `tests/` contains Rust integration tests and shared support.
- `docs/` contains runtime contracts, design records, and current architecture
  notes.
- `builtin_templates/` contains runtime-managed agent templates.
- `docs/website/` contains this mdorigin documentation site.

<!-- INDEX:START -->

- [Create your first agent](./first-agent.md)
  From zero to your first Holon agent: build, start, TUI basics, create an agent, and configure models.
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
