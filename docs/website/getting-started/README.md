---
title: Getting started
summary: Go from zero to your first Holon agent in under 15 minutes.
order: 10
---

# Getting started

Holon ships as an installable release. This section gives you
the shortest path from install to a running agent, then shows where to branch
based on what you want to do next.

## Fastest path

```bash
brew tap holon-run/tap && brew install holon
holon onboard
holon daemon start
```

Open <http://localhost:7878> or run `holon tui`. A successful first session
means you can:

1. send a bounded request to the default main agent;
2. see the agent inspect a real workspace or run an approved tool;
3. receive a concise result or an explicit request for the next decision.

The walkthroughs below cover provider setup, installation, connecting with
the TUI, and creating your first agent.

For the first durable workflow, choose a responsibility with a natural wait:
following a pull request through CI, investigating an issue while waiting for
evidence, or coordinating a release with explicit human approval.

## Which runtime mode should I use?

Holon gives you three ways to interact with the runtime:

| Mode | Command | Best for |
|------|---------|----------|
| **One-shot** | `holon run "..."` | Quick single-turn tasks — no daemon needed |
| **Daemon + TUI** | `holon daemon start` + `holon tui` | Interactive agent sessions with state, queues, and workspaces |
| **Daemon + HTTP** | `holon daemon start` + HTTP client | Integrations, automation, control-plane consumers |

The first agent tutorial uses daemon + TUI because it
gives you the full interactive experience. For one-shot runs, see the
[quick examples](/guides/quick-examples).

## Evaluate or explore?

If you're already familiar with Holon or want to jump straight into specifics:

- **[Quick examples](/guides/quick-examples)** — one-shot and common task patterns
- **[Durable agent workflow](/guides/durable-agent-workflow)** — the full lifecycle of durable agent work
- **[Concepts](/concepts/)** — the mental model before diving into internals
- **[CLI reference](/reference/cli.md)** — full command surface
- **[Troubleshooting](/guides/troubleshooting)** — diagnose common setup issues

## Contribute or develop?

If you plan to modify or contribute to Holon itself:

- **[Local runtime guide](/guides/local-runtime)** — conservative development workflow
- **[Documentation workflow](/guides/documentation-workflow)** — how to build and preview this site
- **[Integration guide](/guides/integration)** — wire Holon into external systems
- Repository `docs/` directory — RFCs, implementation decisions, and architecture notes

## Requirements

- Holon installed on `PATH` (Homebrew or direct binary; see the walkthrough below)
- A model provider API key (Anthropic, OpenAI, or compatible)

## Repository orientation (contributors)

This is a short orientation for contributors. End users don't need to know the repository layout.

- `src/` contains the Rust runtime implementation and executable entrypoints.
- `tests/` contains Rust integration tests and shared support.
- `docs/` contains runtime contracts, design records, and current architecture
  notes.
- `agent_templates/` contains remote-syncable agent templates.
- `docs/website/` contains this mdorigin documentation site.


<!-- INDEX:START -->

- [Create your first agent](./first-agent.md)
  From zero to your first Holon agent: install, start, TUI basics, create an agent, and configure models.
  <!-- mdorigin:index kind=article -->

- [Onboarding guide](./onboarding.md)
  Interactive setup with `holon onboard` — provider, credential, model, and search configuration.
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
