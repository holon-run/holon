# Holon

Holon lets AI agents keep working in your local workspace across prompts,
terminal sessions, and external events.

Most AI agent tools optimize for a conversation surface, a hosted assistant, or
one active coding session. Holon focuses on the lifecycle around the work
itself. It runs as a local, headless server-client runtime: clients can connect,
submit work, disconnect, reconnect, inspect progress, and receive final briefs
while the runtime keeps the work alive.

Each Holon agent owns one durable work session inside that runtime. Its queue,
workspace context, WorkItems, sleep/wake state, execution history, and delivery
path stay attached to the agent instead of being lost with a single client
connection.

Holon is part of the Holon Run local-first AI infrastructure stack.

## Install

Install the latest release with Homebrew:

```bash
brew tap holon-run/tap
brew install holon
holon --help
```

Direct binaries are also available from the
[latest GitHub Release](https://github.com/holon-run/holon/releases/latest) for
Linux amd64, macOS amd64, and macOS arm64.

The command examples below assume `holon` is installed on `PATH`.

## Provider setup

Holon needs a model provider before it can run agent turns. For the fastest
local setup, export a provider key and choose a model:

```bash
# Anthropic-compatible provider
export ANTHROPIC_AUTH_TOKEN="your-api-key"
holon config set model.default "anthropic/claude-sonnet-4-6"

# Or OpenAI Responses
export OPENAI_API_KEY="your-api-key"
holon config set model.default "openai/gpt-5.4"
```

You can inspect the configured provider state with:

```bash
holon config doctor
holon config models list
```

For persistent credentials that avoid shell history and process arguments, use
the credential store and point the provider at that credential profile:

```bash
printf '%s' "$ANTHROPIC_AUTH_TOKEN" \
  | holon config credentials set --kind api_key --stdin anthropic

holon config providers set anthropic \
  --credential-source credential_profile \
  --credential-kind api_key \
  --credential-profile anthropic
```

Holon can also use OpenAI Codex subscription credentials from an existing local
`codex login` session when an `openai-codex/...` model is selected.

## Quickstart

Run a one-shot task in the current repository:

```bash
holon run \
  "inspect this repository, find one failing or missing check, and report the smallest next fix" \
  --json
```

Run against a specific workspace and working directory:

```bash
holon run "analyze this package" \
  --workspace-root /path/to/repo \
  --cwd /path/to/repo/src
```

Start the long-running local runtime:

```bash
holon daemon start
holon agent status
```

Open the local operator console:

```bash
holon tui
```

Stop the daemon:

```bash
holon daemon stop
```

## Feature list

Holon is runtime infrastructure for agent work that must survive beyond one
client session. The main capability surface is:

- **Durable agent sessions**: agents keep their queue, workspace context,
  execution history, and delivery state across client disconnects and later
  reconnects.
- **Runtime WorkItems**: long-running objectives carry a plan, progress
  checklist, blockers, waiting state, and completion report inside the runtime
  instead of living only in chat history.
- **Event-driven continuation**: agents can sleep, wait for callbacks, timers,
  webhooks, task results, or other external events, then wake and continue the
  same work.
- **Local workspace execution**: agents read files, edit code, run commands, and
  verify changes in the repositories you explicitly attach.
- **Worktree-isolated coding work**: coding subtasks and child agents can run in
  managed git worktrees so longer or riskier work stays separate from the main
  checkout.
- **Supervised background tasks and delegation**: agents can delegate commands
  or child agents as explicit tasks, inspect their status and output, and rejoin
  results without losing the parent work context.
- **Local behavior loading**: agents can use repository instructions, agent
  templates, and local skills without being tied to one hosted assistant
  product.
- **Clear trust and delivery boundaries**: Holon preserves input origin and
  trust metadata, separates internal execution traces from user-facing briefs,
  and returns explicit final results.

## Core concepts

Holon is organized around a few runtime primitives:

- `agent`: a long-lived runtime identity with local state
- `WorkItem`: a durable objective with plan, progress, blockers, and completion
  state
- `queue`: all inputs become queued work
- `origin`: each input carries source and trust metadata
- `task`: an execution handle for commands, child agents, and other asynchronous
  work while a WorkItem or turn is being advanced
- `sleep` / `wake`: the runtime can wait and resume from explicit signals
- `workspace`: local repositories are attached and projected explicitly
- `brief`: user-facing output is distinct from internal reasoning and logs

## Common commands

Run local agent work:

```bash
holon run "fix the failing test" --json
holon run "review this repository"
```

Start the runtime in the foreground:

```bash
holon serve
holon serve --access lan --host 192.168.1.10 --port 7878 --token-file ~/.config/holon/remote.token
```

Manage the runtime as a daemon:

```bash
holon daemon start
holon daemon status
holon daemon logs
holon daemon restart
holon daemon stop
```

Inspect local state:

```bash
holon agent list
holon agent status
holon transcript --limit 50
```

## Current release

The current Rust-line release is
[`v0.14.0`](https://github.com/holon-run/holon/releases/tag/v0.14.0).

Highlights:

- more reliable event-driven scheduling
- stronger long-lived WorkItem, task, and agent state
- provider routing and web-search improvements
- TUI, daemon, and remote transport hardening
- improved workspace, worktree, and tool diagnostics

See the [v0.14.0 release notes](https://github.com/holon-run/holon/releases/tag/v0.14.0)
for the full changelog and release assets.

## Status and compatibility

Holon is under active development. The current line is the Rust runtime line,
starting from `v0.13.0`.

The old Go implementation is available as `v0.12.0`, but new runtime work is
happening on the Rust line.

Expect breaking changes while the CLI, daemon, and runtime contracts stabilize.

## Project boundaries

Holon focuses on runtime meaning: agent identity, work continuity, execution
state, local workspace projection, and operator-visible results.

Holon is not:

- a chat UI
- an all-in-one agent platform
- a connector marketplace
- a workflow automation GUI
- a full VM or container sandbox product

Adjacent Holon Run projects cover other layers:

- AgentInbox: source hosting, activation, and delivery
- UXC: unified capability and tool access
- WebMCP Bridge: browser and web-app edge access

When used together, AgentInbox should wake Holon; Holon should decide what the
runtime event means.

## Build from source

For contributors working from a source checkout:

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

## Documentation

Holon's documentation is organized into three layers. See
[documentation layers](docs/website/concepts/documentation-layers.md) for the
full map.

**Using Holon:**

- [Website docs](https://holon.run) — install, getting started, concepts,
  guides, and current reference
- [Security and execution boundaries](docs/website/concepts/security-and-execution-boundaries.md)

**Integrating and operating Holon:**

- [Local operator troubleshooting](docs/local-operator-troubleshooting.md)
- [Release process](docs/release.md)

**Contributing to the runtime:**

- [Architecture overview](docs/architecture-overview.md) — start here
- [RFCs](docs/rfcs/README.md) — canonical design contracts
- [Implementation decisions](docs/implementation-decisions/README.md) — design
  rationale
