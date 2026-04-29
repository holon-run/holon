# Holon

`Holon` is a local-first, headless, event-driven runtime for long-lived agents.

It is built for agents that should keep working across time instead of handling
one prompt and exiting. A `Holon` agent can work in a local workspace, wait for
external changes, wake when needed, resume the same task, coordinate background
work, and report through explicit user-facing output. Runtime posture and
closure outcome stay separate so completion, failure, and waiting reason remain
operator-visible.

The name comes from `漏刻`, the ancient Chinese water clock. The metaphor is
intentional: a system that keeps time, advances in discrete steps, and wakes
work at the right moment.

## One Sentence

`Holon` is a runtime that lets agents keep working in local-first environments.

## Product Boundary

`Holon` should be understood as:

- a runtime
- a control plane
- a local workspace execution layer
- a task orchestration layer
- an event-driven wake / sleep system

It should not be understood as:

- a chat UI
- an all-in-one agent platform
- a connector marketplace
- a workflow automation GUI
- a full VM or container sandbox product

The core problem it solves is not "talking to a model". The core problem is:

`How can an agent keep making progress in a local workspace across time without losing execution boundaries, task state, and trust boundaries?`

## Public Entry Points

`Holon` currently has two primary runtime entry points plus one operator
lifecycle surface.

## Install

The main branch builds a Rust binary named `holon`.

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

### `holon run`

This is the simplest way to use `Holon`.

Use it for:

- one-shot local execution
- coding tasks
- analysis tasks
- quick verification

Example:

```bash
cargo run -- run "fix the failing test" --json
cargo run -- run "fix the failing test" --agent bench-15 --create-agent --json
cargo run -- run "review the release diff" --agent reviewer-1 --create-agent --template reviewer --json
cargo run -- run "continue from the last checkpoint" --agent bench-15 --json
cargo run -- run "analyze this workspace" --workspace-root /path/to/repo --cwd /path/to/repo/src
cargo run -- run "analyze this workspace" --mode analysis
```

`run` now binds workspace execution explicitly and supports two session modes:

- default: use a temporary private agent for one-shot local execution
- `--agent <id>`: target an existing self-owned public agent session
- `--agent <id> --create-agent`: create that self-owned public agent on first
  use and then run on it
- `--template <selector>`: when used together with `--create-agent`, initialize
  the new agent from a reusable template selector

Template selectors currently accept exactly three forms:

- a simple `template_id`, resolved from `~/.agents/templates/<template_id>/`
- an absolute local template directory path
- a GitHub template URL in the form
  `https://github.com/<owner>/<repo>/tree/<ref>/<path-to-template-dir>`

Template application materializes an initial agent-local `AGENTS.md` plus any
declared agent-local skills into the new agent's `agent_home`. After creation,
that agent-local state is the live source of truth; later template changes do
not rewrite existing agents.

When `run` targets a self-owned public agent and no new workspace flags are provided,
it preserves the agent's existing active workspace or worktree session instead
of rebinding it.

`run` now reports the terminal turn's final assistant message directly. Holon
does not force an extra completion-summary round, and it does not synthesize a
generic success summary when the terminal turn produced no assistant text.

`run` binds workspace execution explicitly:

- `workspace_anchor`: `--workspace-root` when provided, otherwise the invocation cwd
- `execution_root`: starts at the selected anchor
- `cwd`: `--cwd` when provided, otherwise the invocation cwd when it stays inside the anchor

### `holon serve`

This is the long-lived runtime mode.

Use it for:

- queue / wake / sleep continuity
- callback, timer, webhook, and control surfaces
- agents that should wait for future events and continue later

Example:

```bash
cargo run -- serve
```

On startup, Holon seeds a small builtin template set into
`~/.agents/templates/` when those directories do not already exist:

- `developer`
- `reviewer`
- `release`

### `holon daemon`

`holon daemon` is the operator-facing lifecycle surface for the same long-lived
runtime that `holon serve` runs in the foreground.

Phase-1 commands:

```bash
cargo run -- daemon start
cargo run -- daemon status
cargo run -- daemon logs
cargo run -- daemon stop
cargo run -- daemon restart
```

Current contract:

- `serve` remains the direct foreground runtime entry point
- `daemon` starts and stops the same runtime shape in the background
- `daemon start` is idempotent for one `HOLON_HOME`
- stale local runtime state is cleaned before start
- socket-path takeover by a different process fails closed instead of silently
  deleting the path
- `daemon stop` prefers graceful shutdown through the local control surface
- `daemon status` exposes local runtime metadata such as `pid`, `home_dir`,
  `socket_path`, and control connectivity
- `daemon status` also reports concise runtime activity:
  active agents, active tasks, and whether the runtime is `idle`, `waiting`,
  or `processing`
- newer daemon clients tolerate older runtimes that do not yet expose the
  activity field
- `daemon status` also surfaces the latest known runtime failure summary when
  available, including timestamp, phase, and a `daemon logs` hint for deeper
  inspection; daemon-level failure files are cleared after a later successful
  `daemon start` or `daemon stop`
- `daemon logs` provides a stable local inspection surface for:
  - the daemon log path
  - the latest known runtime metadata path
  - recent startup/shutdown failure summaries when available
  - a bounded tail of `run/daemon.log`

Workspace binding in `serve` mode is now explicit and inspectable. Use control
commands instead of relying on shell cwd as the runtime source of truth:

```bash
cargo run -- workspace attach /path/to/repo
cargo run -- workspace detach <workspace-id>
cargo run -- workspace exit
```

`workspace exit` only leaves the active execution projection. `workspace
detach` removes a durable binding from the agent's attached workspace set; it
does not delete the workspace directory, registry entry, or task-owned
worktree artifacts. Entering or switching the active workspace/worktree
projection is not exposed as a direct operator control path. Those transitions
should be agent-driven or runtime-owned rather than externally forcing a
running agent into a new execution projection.

### `holon tui`

`holon tui` is a thin local operator console for a running `holon serve`.

- it connects to the local control surface instead of owning `RuntimeHost`
- closing the TUI does not stop timers, tasks, child agents, or waiting state
- phase 1 is for local runtime dogfooding, not a polished product UI
- the main surface is chat-first: type directly, press `Enter` to send
- `/` opens the local TUI command surface: `/help`, `/agents`, `/events`,
  `/tasks`, `/transcript`, `/refresh`, `/clear-status`, `/debug-prompt`
- secondary runtime views open as overlays instead of permanent focused panes:
  `Ctrl+A` agents, `Ctrl+E` events, `Ctrl+T` transcript, `Ctrl+J` tasks
- `?` opens help when the composer is empty
- alternate-screen behavior is configurable with `tui.alternate_screen` set to
  `auto`, `always`, or `never`; `holon tui --no-alt-screen` forces normal
  scrollback mode for the current run

`run`, `serve`, and `daemon` are not different products. They are different
operator surfaces on the same runtime:

- `run` is the lightest local execution mode
- `serve` is the long-lived continuity mode
- `daemon` is the local lifecycle wrapper around long-lived `serve`

For the recommended local troubleshooting order across these entry points, see
[docs/local-operator-troubleshooting.md](docs/local-operator-troubleshooting.md).

## Headline Workflow

The current headline workflow for `Holon` is:

- local-first coding runtime

That means:

- read files in a local workspace
- edit files
- run commands
- verify results
- optionally continue later after an external wake

The recommended way to understand the product today is:

1. start with `holon run`
2. then learn `holon serve`
3. only then move on to callback and inbox-driven continuation flows

## Core Runtime Model

`Holon` is built around a few runtime primitives:

- `queue`: all inputs become queued work
- `origin`: each input carries its source and trust level
- `sleep`: the runtime can suspend until the next useful event
- `wake`: the runtime can resume from an explicit signal or queued event
- `brief`: user-facing output is distinct from internal reasoning
- `task`: long-running or delegated work is modeled explicitly

This runtime shape is designed for:

- long-lived self-owned agents
- event-driven wakeups
- explicit trust boundaries across input origins
- background subtask execution
- clear user-facing output channels

## Current Status

`Holon` is no longer only a paper spec. The repository already contains a
working runtime with:

- multi-agent runtime host
- agent-scoped queue and state
- context window plus deterministic compaction summary
- background task rejoin
- explicit provenance and admission marking on runtime messages
- timer, webhook, callback, and remote ingress
- native tool-use / tool-result runtime loop
- shell-first repo inspection plus local file mutation tools
- host-owned workspace registry and agent workspace attachment
- bounded delegated child-agent support
- managed worktree workflows
- fixture-based regression coverage

Provider support currently includes:

- Anthropic-compatible providers
- OpenAI Responses via `OPENAI_API_KEY`
- Codex subscription via existing local `codex login` credentials

Agent-level model selection is now explicit:

- each long-lived agent can either inherit the runtime default model or carry a
  stable model override
- changing one agent's model does not rewrite the runtime-wide default model
- status surfaces expose the effective model, the effective fallback chain, and
  whether the agent is using an override or the inherited default
- status and run surfaces expose structured token-usage summaries, while
  transcript and audit entries preserve per-turn token usage for later
  consumers such as TUI

## Product Surface Status

### GA candidate

These are closest to the future stable public contract:

- `holon run`
- shell-first local repo inspection
- local shell execution
- local file mutation tools
- analysis and coding prompt modes
- task output and result shaping
- basic transcript, status, and tail inspection

### Preview

These are implemented and usable, but still being tightened as product
surfaces:

- `holon serve`
- `holon daemon`
- callback capability
- timer, webhook, and remote ingress
- background task orchestration
- `SpawnAgent`
- managed worktree workflow

### Experimental

These should not yet be treated as first-line public promises:

- larger parallel worktree orchestration stories
- future remote backends
- stronger sandbox backend matrix
- more complex condition-waiting and subscription runtime semantics

## Trust And Execution Contract

`Holon` does not assume that every input should have the same authority.

At a product level, the current contract is:

- operator input, system input, and external input are distinct
- every runtime message preserves `origin`, `trust`, `delivery_surface`, and
  `admission_context`
- external events may influence wake and continuation
- external events should not silently inherit operator authority
- transport authentication gates admission, but does not by itself rewrite
  runtime authority
- non-message control mutations remain auditable control-plane events instead
  of hidden runtime messages
- execution boundaries should be explained in terms of agent profile plus
  workspace / worktree projection, not per-command approval prompts
- `host_local` is the only implemented execution backend in phase 1
- execution snapshots now expose a policy/capability view so operators can see
  what is hard-enforced today versus only runtime-shaped
- process execution may be exposed, attributed, projected, and gated without
  being strongly sandboxed
- the final restriction matrix for files, tasks, timers, control actions, and
  workspace mutation is deferred to later execution/resource policy work

In other words:

- `Holon` is closer to an execution-profile model for long-lived agents
- it is not primarily a per-command approval UX product

Managed worktrees are also an important part of the safety and review model for
supervised coding flows. They are not just an implementation detail.

The runtime now treats worktree as an execution projection:

- `workspace_anchor` stays pinned to the attached project
- `execution_root` may switch to a managed worktree path
- `active_workspace_entry` makes projection and access mode explicit
- `exclusive_write` means a single writer, not exclusive readers
- `cwd` must stay inside the current execution root
- shell `cd` does not implicitly change workspace attachment

## Local Guidance And Skills

`Holon` now has two stable local guidance roots:

- `agent_home/AGENTS.md`
- `workspace_anchor/AGENTS.md`, with `workspace_anchor/CLAUDE.md` as a
  compatibility fallback only when workspace `AGENTS.md` is absent

Workspace-scoped guidance follows the current attached workspace entry, not
plain shell `cwd` and not the current worktree execution root. `holon debug
prompt` shows the loaded agent/workspace instruction source paths, while
status surfaces only expose instruction source metadata.

It also supports local-first skill discovery rooted at `SKILL.md`.

The current skill contract is intentionally small:

- skills are discovered from local catalogs, not injected wholesale
- the default agent may see user-level skill roots
- named and child agents do not inherit user-level skill catalogs by default
- if a listed skill matches the task, the agent should open that skill's `SKILL.md`
- reading a discovered catalog entry's `SKILL.md` activates that skill
- active skills stay inspectable through status and `holon debug prompt`

## Agent Model

`Holon` now treats `agent` as the runtime primitive and makes identity
boundaries explicit:

- `default agent`: the default operator-facing entrypoint
- `named agent`: an explicitly created long-lived public non-default agent
- `child agent`: a delegated private agent created from another agent

This also comes with explicit visibility, ownership, and profile preset:

- `public` or `private`
- `self_owned` or `parent_supervised`
- `public_named` or `private_child`

Public named agents must be created explicitly:

```bash
cargo run -- agents create release-bot
```

Private child agents are not normal CLI/HTTP targets. They remain inspectable
through parent summaries and debug surfaces while they are active.

## Relationship To AgentInbox

`Holon` is not a connector hub.

When used with `AgentInbox`, the intended split is:

- `Holon` owns runtime meaning
- `AgentInbox` owns source hosting, activation, and delivery

That means:

- `Holon` can participate in event-driven continuation flows with `AgentInbox`
- `Holon` itself should not be described as an inbox or subscription platform

See [docs/agentinbox-wake-only-quickstart.md](docs/agentinbox-wake-only-quickstart.md)
for the current wake-only callback flow.

## Common Commands

Send an operator prompt to a running agent:

```bash
cargo run -- prompt "hello"
cargo run -- prompt --agent alpha "hello from alpha"
```

Run a one-shot local task without starting the daemon:

```bash
cargo run -- run "fix the failing test" --json
cargo run -- run "fix the failing test" --agent bench-15 --create-agent --json
cargo run -- run "continue from the last checkpoint" --agent bench-15 --json
cargo run -- run "analyze this workspace"
```

Inspect runtime state:

```bash
cargo run -- status
cargo run -- agents
cargo run -- agents model get --agent alpha
cargo run -- agents model set anthropic/claude-haiku-4-5 --agent alpha
cargo run -- agents model clear --agent alpha
cargo run -- tail --limit 10
cargo run -- transcript --limit 50
cargo run -- tui
cargo run -- tui --no-alt-screen
```

`holon run --json` includes a top-level `token_usage` object for that run, and
`holon status` includes cumulative token usage plus the most recent turn that
reported token data.

`holon status` is also the intended concise agent-facing inspection surface.
The richer `/state` HTTP snapshot exists for first-party projection bootstrap
clients such as the TUI and is not the generic replacement for the status
surface.

When local troubleshooting is the goal, prefer this order:

1. `holon run --json` for a one-shot reproduction
2. `holon daemon status` for long-lived runtime health
3. `holon daemon logs` for lifecycle or runtime failure details
4. `holon status` / `holon transcript` for agent-scoped inspection
5. `holon tui` for live observation and interaction after the runtime is known
   healthy

Create background work and timers:

```bash
cargo run -- task "demo task" --sleep-ms 1000
cargo run -- timer --after-ms 5000 --summary "wake up"
```

Control an agent:

```bash
cargo run -- control pause
cargo run -- control resume --agent alpha
```

Stopped agents reject new prompts and wakes until an operator explicitly
resumes them. `holon status` and the TUI status/header surfaces include
resume-required lifecycle hints when an agent is administratively stopped.

Inspect or change provider configuration:

```bash
cargo run -- config schema
cargo run -- config get model.default
cargo run -- config set model.default openai/gpt-5.4
cargo run -- config set tui.alternate_screen auto
cargo run -- config doctor
```

`openai-codex/*` now uses the Responses streaming transport path. The Codex
backend requires `stream=true`, so Holon consumes the streaming event feed for
that provider while `openai/*` remains on the single-body Responses JSON path.
The codex streaming request also omits `max_output_tokens`, because the
ChatGPT Codex backend rejects that parameter on
`chatgpt.com/backend-api/codex/responses`.
Configure `openai-codex/*`, `openai/*`, or `anthropic/*` based on the provider
credentials available in your environment.

`config doctor` also reports the current bounded provider retry policy.
Holon retries transient provider request failures up to two times before
continuing to the next configured fallback provider, and fails fast on
deterministic auth / contract / invalid-response errors.
Set `runtime.disable_provider_fallback=true` or export
`HOLON_DISABLE_PROVIDER_FALLBACK=1` to require deterministic single-provider
execution for benchmarking and debugging.
Operator-facing transcript and runtime-failure records now also preserve a
stable provider attempt timeline so retry, fail-fast, fallback, and winning
provider decisions remain inspectable outside the TUI.

Inspect the effective prompt for one agent in local debug mode:

```bash
cargo run -- debug prompt "Explain this workspace"
cargo run -- debug prompt --agent alpha "Fix the bug"
```

`debug prompt --agent ...` only inspects an existing agent identity. It does
not create named agents implicitly.

Run tests:

```bash
cargo test
```

Run the live provider tests manually:

```bash
cargo test --test live_anthropic -- --ignored
OPENAI_API_KEY=... cargo test --test live_openai live_openai_provider_returns_real_response -- --ignored
cargo test --test live_codex live_openai_codex_provider_returns_real_response -- --ignored
HOLON_LIVE_OPENAI_CODEX_MODEL=gpt-5.3-codex-spark cargo test --test live_codex live_openai_codex_provider_returns_tool_call_for_real_schema -- --ignored
```

## Benchmarking

Run the first benchmark wave:

```bash
cd benchmark
npm install
cd ..
node benchmark/run.mjs --label baseline-preprompt
node benchmark/run.mjs --label prompt-v2
node benchmark/run.mjs compare --baseline baseline-preprompt --candidate prompt-v2
```

Run a real-repo benchmark task:

```bash
cd benchmark
npm install
cd ..
node benchmark/run.mjs validate-manifest --manifest benchmarks/tasks/holon-0015-tool-guidance-registry.yaml
node benchmark/run.mjs real --manifest benchmarks/tasks/holon-0015-tool-guidance-registry.yaml --runner holon-openai --runner codex-openai --label bench-local-0015
node benchmark/run.mjs suite --suite benchmarks/suites/openai-phase1.local.yaml --label bench-openai-phase1
```

Live provider tests are ignored by default. Anthropic-compatible tests use local
config from `~/.claude/settings.json` when environment variables are not
already set. Codex subscription support reuses local `codex` CLI auth state
from `~/.codex/auth.json` or the platform keychain.

## Repository Layout

- `docs/`: architecture notes, goals, and design records
- `src/`: runtime implementation
- `tests/`: integration and workflow coverage
- `benchmark/`: fixture benchmark harness
- `benchmarks/`: real-repo benchmark manifests and suites

## Key Docs

See [docs/project-goals.md](docs/project-goals.md) for the original project
scope and [docs/runtime-spec.md](docs/runtime-spec.md) for the first runtime
contract.

See [docs/architecture-overview.md](docs/architecture-overview.md) for the current runtime
shape and [docs/next-phase-direction.md](docs/next-phase-direction.md) for the
structural direction that followed the first coding-capable pass.

See [docs/rfcs/result-closure.md](docs/rfcs/result-closure.md) for the current
closure-outcome RFC that distinguishes completion, waiting, and runtime
posture.

See [docs/rfcs/continuation-trigger.md](docs/rfcs/continuation-trigger.md) for
the current RFC on continuation trigger kinds, waiting-reason matching, and
the boundary between wake and model-visible continuation. The current runtime
now derives typed continuation resolution at message-processing time, records
the most recent decision as `last_continuation`, and treats blocking
`TaskResult` as the canonical rejoin point.

See
[docs/rfcs/objective-delta-and-acceptance-boundary.md](docs/rfcs/objective-delta-and-acceptance-boundary.md)
for the current RFC on preserving current scope across follow-up, delegation,
and resume. The current runtime now persists `objective_state` on each agent,
including `current_objective`, `last_delta`, and `acceptance_boundary`, and
surfaces that state through status and `holon debug prompt`.

See
[docs/rfcs/default-trust-auth-and-control.md](docs/rfcs/default-trust-auth-and-control.md)
for the current RFC on default trust mapping, mutation authority, and control
surfaces.

See
[docs/rfcs/execution-policy-and-virtual-execution-boundary.md](docs/rfcs/execution-policy-and-virtual-execution-boundary.md)
for the current RFC on how resource authority, execution policy, and virtual
execution capabilities should be separated and recombined.

See
[docs/rfcs/workspace-binding-and-execution-roots.md](docs/rfcs/workspace-binding-and-execution-roots.md)
for the current RFC on `workspace_anchor`, workspace attachment, execution
roots, and cwd binding.

See
[docs/rfcs/instruction-loading.md](docs/rfcs/instruction-loading.md)
for the current RFC on `AGENTS.md` loading roots, `CLAUDE.md` fallback, and
the boundary between stable instruction roots and shell cwd.

See
[docs/rfcs/workspace-entry-and-projection.md](docs/rfcs/workspace-entry-and-projection.md)
for the current RFC on `UseWorkspace`, active workspace selection,
execution-root projection, and minimal occupancy semantics.

See [docs/rfcs/agent-profile-model.md](docs/rfcs/agent-profile-model.md)
for the current RFC on agent profile presets, visibility, and ownership.

See [docs/rfcs/agent-control-plane-model.md](docs/rfcs/agent-control-plane-model.md)
for the current RFC on `AgentGet`, `SpawnAgent`, supervision, and agent-plane
contracts.

See
[docs/rfcs/skill-discovery-and-activation.md](docs/rfcs/skill-discovery-and-activation.md)
for the current RFC on skill catalogs, agent attachment, file-based skill use,
and activation state across compaction and resume.

See [docs/coding-roadmap.md](docs/coding-roadmap.md) for the coding-agent
roadmap and [docs/worktree-design-roadmap.md](docs/worktree-design-roadmap.md)
for the worktree workflow design.

See [docs/benchmark-plan.md](docs/benchmark-plan.md),
[benchmarks/README.md](benchmarks/README.md), and
[docs/benchmark-results.md](docs/benchmark-results.md) for benchmark design and
results.

See [docs/implementation-decisions/README.md](docs/implementation-decisions/README.md)
for concrete design choices taken during implementation.
