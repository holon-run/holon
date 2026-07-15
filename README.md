# Holon

English | [中文](README.zh-CN.md)

[![Release](https://img.shields.io/github/v/release/holon-run/holon?sort=semver)](https://github.com/holon-run/holon/releases/latest)[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Holon is a **local workbench for agents doing continuous work**.

Holon itself is not an agent. It provides a local working environment for multiple agents. Agents understand goals and drive execution; Holon treats "work" as the core unit, preserving state, organizing context, recording waits and wakes, so tasks that span sessions, commands, human confirmation, or external events can resume at the right time and eventually deliver results back to the operator.

## What does Holon provide?

| Capability | What it means |
|---|---|
| **Continuous agent workspace** | Each agent has its own continuous working context in Holon, instead of restarting with every terminal, request, or client connection. |
| **Work-first task model** | Holon organizes tasks, waits, execution progress, and final delivery as explicit Work, instead of leaving them scattered across conversations. |
| **Event-driven wait and wake** | Agents can wait for task results, external events, or operator input, then return to the corresponding work when the condition is satisfied. |
| **Explicit context and trust boundaries** | Holon distinguishes operator input, external events, tool results, and internal execution traces so information from different origins is not mixed together. |
| **Local-first execution environment** | Holon is built for local repositories, shell, worktrees, and development toolchains, letting agents execute tasks in the real working environment. |

> Keep agent work alive in your local workspace.

## Quickstart

Holon provides two interaction modes: **TUI** (terminal) and **Web GUI** (browser).

### 1. Install

```bash
brew tap holon-run/tap && brew install holon
```

Or download binaries from [GitHub Releases](https://github.com/holon-run/holon/releases/latest).

### 2. Configure a provider

```bash
holon onboard
```

This walks through provider credential setup interactively. You can also
configure providers through the Web GUI **Settings** page after starting the
daemon. See [Configuration Reference](docs/website/reference/configuration.md)
and [Web GUI guide](docs/website/guides/web-gui.md) for more.

### 3. Start the daemon

```bash
holon daemon start
```

### 4a. TUI (terminal)

```bash
holon tui
```

Select an agent and start working. Agents keep running after you disconnect.

### 4b. Web GUI (browser)

Open <http://localhost:7878>. Create an agent and work through a chat interface
with built-in file browser, task tracking, and more.

For more: [TUI guide](docs/website/guides/tui.md) · [Web GUI guide](docs/website/guides/web-gui.md) · [First agent](docs/website/getting-started/first-agent.md)

## Install

```bash
brew tap holon-run/tap
brew install holon
holon --help
```

You can also download prebuilt binaries for Linux amd64, macOS amd64, and macOS
arm64 from [GitHub Releases](https://github.com/holon-run/holon/releases/latest).

The examples below assume `holon` is installed on `PATH`.

## Provider setup

Holon needs a model provider before it can run agents. The recommended path is:

- **`holon onboard`** — interactive CLI setup that guides you through provider
  credential configuration without echoing secrets.
- **Web GUI Settings** — after starting the daemon, open
  <http://localhost:7878> and configure providers through the Settings page.

Holon supports common providers such as Anthropic, OpenAI, DeepSeek, OpenRouter,
Qwen, GLM, Xiaomi, Kimi, and MiniMax. For advanced setup including credential
profiles, custom providers, and Codex subscriptions, see
[Configuration Reference](docs/website/reference/configuration.md) and
[Supported Models](docs/website/reference/models.md).

## Core concepts

Holon breaks agent work into a few explicit runtime objects:

- **Agent** — long-lived local identity with its own queue, state, and working
  context.
- **WorkItem** — continuously advanceable goal with a plan, progress, blockers,
  wait conditions, and a completion report.
- **Task** — supervised asynchronous execution (command, background task, or
  child agent).
- **WaitFor / wake** — explicit declaration of waiting for a task result,
  external event, or operator input, and resuming when the condition is
  satisfied.
- **Workspace / worktree** — execute in local repositories and isolate coding
  tasks into managed worktrees.
- **Origin / brief** — preserves input origin and trust information while
  keeping execution traces separate from operator-visible delivery.

For more detailed explanations, see [Concepts](docs/website/concepts/).

## Status and compatibility

Holon is under active development. The current recommended release is
[`v0.29.1`](https://github.com/holon-run/holon/releases/tag/v0.29.1).

The current project focus remains the Rust runtime: agent lifecycle, queues,
WaitFor/wake, tasks, WorkItems, trust boundaries, local workspaces, and
structured delivery.

## Documentation

- [Website docs](https://holon.run) — install, getting started, concepts, guides, reference
- [Documentation layers](docs/website/concepts/documentation-layers.md)
- [Architecture overview](docs/architecture-overview.md)
- [RFCs](docs/rfcs/README.md)
- [Implementation decisions](docs/implementation-decisions/README.md)
- [Release process](docs/release.md)

## Build from source

The Rust binary embeds web GUI assets at compile time via `rust-embed`. Build
the frontend first, then compile the binary:

```bash
make all
holon --help
```

Or step by step:

```bash
make web    # build web GUI (requires Node.js 24 LTS)
make build  # build Rust binary
```

## Development

Use Node.js 24 LTS for Web GUI development. Run the same full validation used
by CI with `make`:

```bash
make ci
```

For a focused Web GUI check, including Vitest and the production build:

```bash
make web-ci
```

See `make help` for the full list of targets.

Run the benchmark harness:

```bash
cd benchmark
npm install
npm test
```

## Community

- [GitHub Discussions](https://github.com/holon-run/holon/discussions)
- [GitHub Issues](https://github.com/holon-run/holon/issues)

## License

This project is licensed under the [Apache-2.0](LICENSE) license.
