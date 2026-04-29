# Holon Project Goals

## One Sentence

Holon is a headless runtime for agents that stay alive, wait for events, and
resume useful work without losing control of trust boundaries.

## Problem

Most agent tooling still assumes a request-response lifecycle:

- one prompt comes in
- one agent run happens
- the process exits or stalls

That model breaks down when the agent should:

- keep context across time
- react to external messages or webhooks
- pause and resume work intentionally
- run background subtasks
- separate operator-visible communication from internal execution traces

## Product Goal

Build a small runtime that makes long-lived, event-driven agents practical.

The first version should make these flows explicit instead of hiding them in
framework magic:

- input intake
- trust classification
- message delivery surface and admission posture
- closure outcome and waiting reason
- continuation trigger classification and wake-versus-resume boundaries
- current objective, scope delta, and acceptance boundary
- workspace attachment
- active workspace entry, projection kind, and access mode
- execution root and cwd binding
- per-agent model posture and inherited-default versus override state
- explicit execution-policy and capability boundary reporting
- explicit local daemon lifecycle state for the long-lived runtime
- explicit local runtime activity state for daemon inspection:
  `idle` / `waiting` / `processing`
- explicit last-runtime-failure summary for daemon inspection before dropping
  into logs
- explicit local daemon log inspection path for startup/shutdown debugging
- one documented local troubleshooting order across `run`, `daemon status`,
  `daemon logs`, `status` / `transcript`, `tui`, and foreground `serve`
- explicit provider-attempt timeline diagnostics for retry, fail-fast, and
  fallback behavior on operator-facing runtime surfaces
- explicit operator-facing token usage summaries for run/status plus per-turn
  token usage on stable transcript and audit surfaces
- shell-first local repo inspection with bounded command-output reinjection
- prompt queueing
- wake / sleep transitions
- task scheduling
- background execution
- user-facing delivery

## Design Principles

### 1. Headless First

The runtime should work without a terminal UI. Any future UI is an adapter, not
the core.

That still allows a thin local operator console as an adapter on top of the
runtime control surface. The console should not own runtime lifecycle.
When that console exists, it should keep prompting simple and move secondary
inspection into temporary views instead of requiring a multi-pane focus model.

### 2. Explicit Origins

Messages from an operator, timer, webhook, or external channel are not the same
thing. The runtime should model that directly.

### 3. Separate Thinking From Delivery

Internal reasoning, tool traces, and user-facing updates should not be the same
channel.

### 4. Sleep Is A First-Class State

The runtime should know when nothing useful should happen, and suspend cleanly
until the next wake condition.

### 5. Background Work Must Stay Observable

If the runtime delegates or spawns tasks, it should preserve task identity,
state, and reporting.

## First Build Scope

The first build should include:

- a single local runtime process
- one in-memory or file-backed queue
- one session state model
- operator input ingress
- timer or cron-like wakeup
- one external event ingress path
- structured `brief` output
- one background task abstraction

## Out Of Scope For V1

The first build should not require:

- multi-tenant hosting
- browser UI
- marketplace plugins
- distributed task workers
- hard security sandboxing
- deep model-provider abstraction layers

## Key Open Questions

- What is the minimal message envelope for `origin`, `trust`, and `priority`?
- Which ingress facts belong on queued messages versus control-plane audit?
- When should the runtime self-wake versus stay asleep?
- How should background tasks rejoin the main session?
- Which later execution/resource policy checks belong in the runtime versus in
  tools?
- How much working memory should be preserved before compaction?

## First Deliverable

Before building integrations, Holon should produce one clear runtime spec for:

- session state
- queue item types
- event ingress
- wake / sleep lifecycle
- brief output contract
- task orchestration contract

That first draft now lives in [runtime-spec.md](runtime-spec.md).
