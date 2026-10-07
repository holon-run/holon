# Holon

English | [中文](README.zh-CN.md)

[![Release](https://img.shields.io/github/v/release/holon-run/holon?sort=semver)](https://github.com/holon-run/holon/releases/latest)[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

**Agents that act and follow through**

Holon is a local workbench for agents handling ongoing work—not just one-off prompts. It keeps goals, plans, progress, and wait conditions in explicit WorkItems, so work can continue across sessions, commands, human input, and external events.

When the next condition is satisfied, the agent resumes the right work in the right workspace and delivers the result back to you.

Use Holon when a task should keep moving after you close the terminal, wait for a command result, ask for review, or receive an external event.

Holon itself is not an agent. It provides a local working environment for multiple agents. Agents understand goals and drive execution; Holon treats "work" as the core unit, preserving state, organizing context, recording waits and wakes, so tasks can resume at the right time and eventually deliver results back to the operator.

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
and [Web GUI guide](docs/website/guides/use-web-gui.md) for more.

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

For more: [TUI guide](docs/website/reference/tui.md) · [Web GUI guide](docs/website/guides/use-web-gui.md) · [First agent](docs/website/getting-started/first-agent.md)

## Local App reference

The reference Local App fixture lives in
`examples/local-app/agent-workbench`. Copy it into an Agent's
`agent_home/apps/` directory as `agent-workbench`, start Holon, and open:

```text
/apps/<agent-id>/agent-workbench/
```

It demonstrates the Local App lifecycle: discovery, static HTML/CSS/JavaScript
hosting, `Holon.context()`, `Holon.request()`, and `Holon.events`. The
end-to-end fixture test can be run with:

```bash
cargo test --test http_apps apps_reference_workbench_end_to_end
```

Hosted apps load the browser artifact from
`/apps/<agent-id>/<app-id>/holon.js`. The artifact installs `window.Holon` and
uses the existing same-origin session boundary; App requests do not become
operator input and do not grant additional permissions.

## Install

```bash
brew tap holon-run/tap
brew install holon
holon --help
```

You can also download prebuilt binaries for Linux amd64, macOS amd64, and macOS
arm64 from [GitHub Releases](https://github.com/holon-run/holon/releases/latest).
The Linux amd64 binary supports Ubuntu 22.04 or newer (glibc 2.35 or newer).

macOS 13 or later users on Apple Silicon or Intel Macs can install the
universal `Holon-<version>.dmg` from the same release page. The native menu
bar app controls the same daemon as the CLI, supports
launch at login, and can install its bundled CLI into `~/.local/bin` without
overwriting another installation.

The examples below assume `holon` is installed on `PATH`.

### Docker

Release images are published to GitHub Container Registry. The container runs
`holon serve` in the foreground and requires a control token because it listens
on a non-loopback address:

```bash
docker run --rm \
  -p 127.0.0.1:7878:7878 \
  -e HOLON_CONTROL_TOKEN='replace-with-a-long-random-token' \
  -e HOLON_MODEL='openai/gpt-5.4' \
  -e OPENAI_API_KEY \
  -v holon-home:/var/lib/holon \
  ghcr.io/holon-run/holon:latest
```

Replace the model and credential environment variable when using another
provider. Holon validates that the configured model provider is available
before the service starts. Scheduling is canonical-only. A legacy selector is
rejected at startup; use Holon v0.31.1 with a pre-migration database backup if
legacy rollback is required.

The base image includes Git and common shell/network utilities. Mount a
writable workspace at `/workspace`, or derive a project-specific image when
the agent needs additional development toolchains. The published image is
currently Linux amd64.

For a browser-first local trial without a provider configured yet, use the
checked-in Compose entrypoint:

```bash
docker compose up --build
```

It persists Holon state and the workspace in named volumes, starts with
`HOLON_BOOTSTRAP=1`, and redirects the browser to
<http://localhost:7878/settings>. Configure a provider credential and default
model there; task and tool mutations remain blocked until the runtime reports a
ready default provider. Set `HOLON_CONTROL_TOKEN` before exposing the port
beyond the local machine.

For a Railway Service or Template deployment, see
[Railway deployment](docs/deployment-railway.md). It uses the same Docker
image and bootstrap onboarding flow, maps the injected Railway `PORT`, and
keeps runtime state plus the workspace on one Railway Volume.

For release-level container smoke and optional real-LLM workspace/WorkItem
acceptance cases, see
[Docker release acceptance](docs/testing/docker-acceptance.md).

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
[`v0.48.0`](https://github.com/holon-run/holon/releases/tag/v0.48.0).

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

Building the Rust binary requires Rust 1.85 or newer. WebFetch's browser TLS
fingerprint support uses the Apache-2.0-licensed `wreq` and `wreq-util` crates,
which build BoringSSL from source and require CMake, Perl, and Clang/libclang
on the build host.

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
npm ci
npm test
```

For the fast fixture guardrail subset:

```bash
npm run guardrails
```

See the [benchmark plan](docs/benchmark-plan.md) for the implemented task,
runner, verifier, and artifact contracts. Real-repo task manifests and suites
are documented in [`benchmarks/README.md`](benchmarks/README.md), with commands
for `validate-manifest`, `real`, and `suite`.

The default [Benchmark CI workflow](.github/workflows/benchmark.yml) runs the
Node harness tests and a stable Rust performance subset. It does not run
real-repo suites; those remain explicitly authorized operator commands. The
Rust performance job uploads `target/performance/` artifacts, while local
Node runs write their results under `.benchmark-results/`.

## Community

- [GitHub Discussions](https://github.com/holon-run/holon/discussions)
- [GitHub Issues](https://github.com/holon-run/holon/issues)

## License

This project is licensed under the [Apache-2.0](LICENSE) license.
