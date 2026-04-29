# Holon

Holon is a local-first AI agent runtime for long-running work in local
workspaces.

It is built for agents that should keep working across time: read and edit a
local repository, run commands, wait for external changes, wake up later, and
return explicit operator-facing results.

The project is part of the Holon Run local-first AI infrastructure stack.

## Current Status

Holon is under active development.

The current `main` branch is a Rust rewrite of the original Holon line. It
intentionally replaces the earlier Go implementation and does not guarantee
compatibility with older `main` snapshots or Go-line behavior.

If you need the old Go implementation, use the latest Go-line release
(`v0.12.0`). The Rust line starts from `v0.13.0`.

Expect breaking changes while the runtime, CLI, and daemon surfaces are being
stabilized.

## What Holon Is

Holon is:

- a local-first runtime for AI agents
- a headless execution layer for local workspaces
- a control plane for agent state, tasks, wakeups, and daemon lifecycle
- a foundation for coding agents, review agents, and event-driven continuation

Holon is not:

- a chat UI
- an all-in-one agent platform
- a connector marketplace
- a workflow automation GUI
- a full VM or container sandbox product

The core question Holon explores is:

> How can an agent keep making progress in a local workspace across time
> without losing execution boundaries, task state, or trust boundaries?

## Install

Build from source:

```bash
cargo install --path .
holon --help
```

Released binaries are published as GitHub Release assets for Linux amd64,
macOS amd64, and macOS arm64. Once a release is tagged, install with Homebrew:

```bash
brew tap holon-run/tap
brew install holon
```

## Core Commands

Run a one-shot local task:

```bash
holon run "fix the failing test" --json
holon run "review this repository" --mode analysis
holon run "analyze this workspace" --workspace-root /path/to/repo --cwd /path/to/repo/src
```

Start the long-running runtime in the foreground:

```bash
holon serve
```

Manage the runtime as a local daemon:

```bash
holon daemon start
holon daemon status
holon daemon logs
holon daemon stop
```

Open the local operator console:

```bash
holon tui
```

Inspect state:

```bash
holon status
holon agents
holon transcript --limit 50
```

## Runtime Model

Holon is organized around a few runtime primitives:

- `agent`: a long-lived runtime identity with local state
- `queue`: all inputs become queued work
- `origin`: each input carries source and trust metadata
- `task`: long-running or delegated work is modeled explicitly
- `sleep` / `wake`: the runtime can wait and resume from explicit signals
- `workspace`: local repositories are attached and projected explicitly
- `brief`: operator-facing output is distinct from internal reasoning

The runtime currently supports:

- local file inspection and mutation
- shell-first repository work
- agent-scoped queues and state
- daemon lifecycle management
- timers, callbacks, webhooks, and remote ingress
- background task orchestration
- managed worktree workflows
- local skill and instruction loading
- Anthropic-compatible providers
- OpenAI Responses via `OPENAI_API_KEY`
- Codex subscription use through existing local `codex login` credentials

## Project Boundaries

Holon focuses on runtime meaning: agent identity, task continuity, execution
state, local workspace projection, and operator-visible results.

Adjacent Holon Run projects cover other layers:

- AgentInbox: source hosting, activation, and delivery
- UXC: unified capability and tool access
- WebMCP Bridge: browser and web-app edge access

When used together, AgentInbox should wake Holon; Holon should decide what the
runtime event means.

## Development

Run checks:

```bash
cargo fmt --all -- --check
cargo test --all-targets -- --test-threads=1
```

Run the benchmark harness:

```bash
cd benchmark
npm install
npm test
```

Useful docs:

- [Architecture overview](docs/architecture-overview.md)
- [Runtime spec](docs/runtime-spec.md)
- [Release process](docs/release.md)
- [RFCs](docs/rfcs/README.md)
- [Implementation decisions](docs/implementation-decisions/README.md)
- [Local operator troubleshooting](docs/local-operator-troubleshooting.md)
- [Benchmark design](docs/benchmark-plan.md)
