# Holon

English | [中文](README.zh-CN.md)

[![Release](https://img.shields.io/github/v/release/holon-run/holon?sort=semver)](https://github.com/holon-run/holon/releases/latest)[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Holon is a **local workbench for agents doing continuous work**.

Holon itself is not an agent. It provides a local working environment for multiple agents. Agents understand goals and drive execution; Holon treats "work" as the core unit, preserving state, organizing context, recording waits and wakes, so tasks that span sessions, commands, human confirmation, or external events can resume at the right time and eventually deliver results back to the operator.

## Table of contents

- [What does Holon provide?](#what-does-holon-provide)
- [Install](#install)
- [Provider setup](#provider-setup)
- [Quickstart](#quickstart)
- [Core concepts](#core-concepts)
- [Status and compatibility](#status-and-compatibility)
- [Project boundaries](#project-boundaries)
- [Documentation](#documentation)
- [Build from source](#build-from-source)

## What does Holon provide?

| Capability | What it means |
|---|---|
| **Continuous agent workspace** | Each agent has its own continuous working context in Holon, instead of restarting with every terminal, request, or client connection. |
| **Work-first task model** | Holon organizes tasks, waits, execution progress, and final delivery as explicit Work, instead of leaving them scattered across conversations. |
| **Event-driven wait and wake** | Agents can wait for task results, external events, or operator input, then return to the corresponding work when the condition is satisfied. |
| **Explicit context and trust boundaries** | Holon distinguishes operator input, external events, tool results, and internal execution traces so information from different origins is not mixed together. |
| **Local-first execution environment** | Holon is built for local repositories, shell, worktrees, and development toolchains, letting agents execute tasks in the real working environment. |

> Keep agent work alive in your local workspace.

## Install

Install the latest release with Homebrew:

```bash
brew tap holon-run/tap
brew install holon
holon --help
```

You can also download prebuilt binaries for Linux amd64, macOS amd64, and macOS
arm64 from [GitHub Releases](https://github.com/holon-run/holon/releases/latest).

The examples below assume `holon` is installed on `PATH`.

## Provider setup

Holon needs a model provider before it can run agents. It mainly supports three
setup paths:

- **Local credential storage**: recommended for daily use. Manage API keys
  through credential profiles, avoiding dependence on environment variables that
  must be injected before the daemon starts.
- **Built-in providers**: supports common providers such as Anthropic, OpenAI,
  DeepSeek, OpenRouter, Qwen, GLM, Xiaomi, Kimi, and MiniMax.
- **External login / custom providers**: `openai-codex/...` can reuse a local
  `codex login` session and supports Codex subscriptions. You can also connect
  custom providers with compatible protocols.

For an interactive first run or repair flow, use:

```bash
holon onboard
```

In a TTY this guides you through the default provider credential setup without
echoing the credential material. In scripts, use `holon onboard --json` for the
secret-safe diagnostic report.

The equivalent manual setup is to save the API key first, then point the
provider at the corresponding credential profile:

```bash
printf '%s' "$DEEPSEEK_API_KEY" \
  | holon config credentials set --kind api_key --stdin deepseek

holon config providers set deepseek \
  --credential-source credential_profile \
  --credential-kind api_key \
  --credential-profile deepseek

holon config set model.default "deepseek/deepseek-v4-pro"

# Or use a local Codex login session / Codex subscription
holon config set model.default "openai-codex/gpt-5.5"
```

Inspect the configured state with:

```bash
holon onboard
holon config doctor
holon config models list
```

For more about providers, credential profiles, custom providers, and the model
catalog, see:

- [Configuration Reference](docs/website/reference/configuration.md)
- [Supported Models](docs/website/reference/models.md)

## Quickstart

### 1. Start the daemon

Start the long-running local runtime first:

```bash
holon daemon start
holon daemon status
```

### 2. Connect the TUI

Connect the TUI:

```bash
holon tui
```

### 3. Select or create an agent

Holon automatically provides a default main agent. There are two ways to create
a new agent:

- Tell the main agent in the TUI and let it create one for you.
- Or create one through the CLI:

```bash
holon agent create builder --template holon-developer
holon agent list
```

After that, select an agent in the TUI and start working. After the TUI
disconnects, the agent continues running in the daemon.

For more operations, see the [TUI command reference](docs/website/reference/cli.md#terminal-ui)
and [Daemon management](docs/website/reference/cli.md#daemon-management).

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

For more detailed explanations, see [Concepts](docs/website/concepts/).

## Current release

The current recommended release is
[`v0.16.0`](https://github.com/holon-run/holon/releases/tag/v0.16.0).

`v0.15.0` is the baseline release where the Holon Rust runtime enters public
compatibility maintenance. Starting from this version, the project will maintain
compatibility for the CLI, daemon/API semantics, and local persistent storage.

See the full changes in the
[v0.16.0 Release Notes](https://github.com/holon-run/holon/releases/tag/v0.16.0).

## Status and compatibility

Holon is under active development. Starting from `v0.15.0`, the project treats
the following surfaces as public contracts that need compatibility maintenance:

- **CLI**: common commands, arguments, and structured output should remain
  migratable; breaking changes should be documented with release notes and
  migration paths.
- **Interfaces**: daemon client APIs, event semantics, and runtime object fields
  should remain backward compatible or provide clear versioned evolution paths.
- **Persistent storage**: local data such as agent state, ledgers, messages,
  transcripts, WorkItems, and tasks should support upgrades and read
  compatibility.

The current project focus remains the Rust runtime: agent lifecycle, queues,
WaitFor/wake, tasks, WorkItems, trust boundaries, local workspaces, and
structured delivery.

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

## Documentation

Holon's documentation is organized into three layers. See
[documentation layers](docs/website/concepts/documentation-layers.md).

**Using Holon:**

- [Website docs](https://holon.run) — install, getting started, concepts,
  guides, and current reference
- [Security and execution boundaries](docs/website/concepts/security-and-execution-boundaries.md)

**Integrating and operating Holon:**

- [Local operator troubleshooting](docs/local-operator-troubleshooting.md)
- [Release process](docs/release.md)

**Contributing to the runtime:**

- [Architecture overview](docs/architecture-overview.md) — start here
- [RFCs](docs/rfcs/README.md) — specification and design contracts
- [Implementation decisions](docs/implementation-decisions/README.md) — design
  rationale

## Community

- [GitHub Discussions](https://github.com/holon-run/holon/discussions)
- [GitHub Issues](https://github.com/holon-run/holon/issues)

## Build from source

```bash
cargo install --path .
holon --help
```

## Development

Run checks:

```bash
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo check --all-targets
cargo test --all-targets -- --test-threads=1
```

Run the benchmark harness:

```bash
cd benchmark
npm install
npm test
```

## License

This project is licensed under the [Apache-2.0](LICENSE) license.
