# Holon Roadmap

This document defines the staged roadmap for `Holon`.

The purpose of the roadmap is to keep the project focused on the runtime core
instead of expanding surface area too early.

## Guiding Rule

`Holon` should grow in this order:

1. stable runtime core
2. context and memory
3. background work
4. trust and policy
5. external event surfaces
6. multi-session and longer-horizon memory

Each stage should have a clear definition of done before the next stage expands
scope.

## M0: Stabilize The Current MVP

### Goal

Turn the current single-session Rust MVP into a reliable baseline.

### Includes

- stabilize `serve`, `prompt`, `status`, and `tail`
- add an explicit `holon daemon` lifecycle surface on top of `serve`
- add a thin local TUI on top of `serve` for runtime dogfooding
- simplify the local TUI into a chat-first operator surface with overlay-based
  secondary views
- tighten startup and runtime error messages
- make JSONL persistence and restore behavior predictable
- make workspace attachment and execution-root state explicit
- make active workspace entry, projection, and occupancy explicit
- make closure outcome and waiting reason explicit
- make continuation trigger classification explicit
- make objective, delta, and acceptance boundary state explicit
- make per-agent model override and effective-model status explicit
- make message provenance include delivery surface and admission context
- make host-local execution policy and capability boundaries explicit
- make repo inspection shell-first and bound command output before model reinjection
- document how local config is loaded and how live tests run
- keep the current single-session lifecycle easy to reason about

### Definition Of Done

- `cargo test` passes locally
- real Anthropic live tests pass locally
- restart recovery preserves basic session state
- CLI commands behave consistently and are documented
- local runtime lifecycle is inspectable through `daemon start|stop|status|restart`
- `holon daemon logs` provides a first-class local inspection path for daemon
  lifecycle failures
- `daemon status` can distinguish healthy-idle, healthy-waiting, and
  healthy-processing runtime states
- `daemon status` can surface the most recent runtime failure summary before
  operators need to inspect logs
- provider retry / fail-fast / fallback history is inspectable through stable
  transcript and audit diagnostics
- per-turn token usage is inspectable through stable runtime-facing run,
  status, transcript, and audit surfaces
- local operator troubleshooting follows one documented path across `run`,
  `daemon status`, `daemon logs`, `status` / `transcript`, `tui`, and
  foreground `serve`
- repo inspection defaults to shell-first `exec_command` patterns instead of
  provider-facing `Read` / `Glob` / `Grep`
- `holon tui` can continuously observe and drive a running local runtime
- `holon tui` no longer requires page/tab/pane focus switching for normal
  prompting and inspection
- no known correctness bugs in queue ordering, sleep/wake, or brief creation

### Explicitly Not Included

- new transports
- multi-session support
- complex task execution
- context compaction

## M1: Context Management V1

### Goal

Move from “current message only” to a minimal but real multi-turn context model.

### Includes

- add a model-visible context builder
- include a recent `N`-message window in requests
- include relevant `brief` records in model-visible context
- preserve `origin`, `trust`, and `kind` in context rendering
- keep full JSONL history as the durable audit log
- separate “durable history” from “active model context”

### Definition Of Done

- the second user message can reference the first result
- webhook and operator messages remain distinguishable in context
- brief outputs are visible to future turns when relevant
- context assembly is covered by unit tests
- no compaction yet; just a bounded window with deterministic rules

### Explicitly Not Included

- summary generation
- memory compaction
- snipping or collapse logic

## M2: Tasks And Background Execution V1

### Goal

Turn the current task placeholders into a real runtime feature.

### Includes

- formalize task lifecycle: `queued`, `running`, `completed`, `failed`, `cancelled`
- add a background job abstraction
- make task updates re-enter the main queue as `task_status` and `task_result`
- expose active task state via runtime and API
- generate formal `brief` output for task completion or failure

### Definition Of Done

- a long-running task can execute without blocking the main session loop
- task state is queryable through `status`
- task completion re-enters the queue and updates session state correctly
- task failures remain auditable and visible to the operator

### Explicitly Not Included

- remote workers
- worktree isolation
- multi-agent orchestration

## M3: Trust And Policy V1

### Goal

Freeze provenance and admission marking before the final execution/resource
policy is designed.

### Includes

- define default trust policy by origin
- preserve `delivery_surface` for message-producing ingress
- preserve `admission_context` for message and control-plane admission
- preserve admission and trust marks in logs and audit records
- make it inspectable how a message or control action entered the runtime

### Definition Of Done

- operator, webhook, channel, system, and task inputs follow distinct marking
- external input cannot silently inherit operator authority
- admission and provenance fields are testable and logged
- runtime decisions are inspectable after the fact without freezing the final
  restriction matrix

### Explicitly Not Included

- OS sandboxing
- full permission UI
- enterprise policy surface
- final allow/deny rules for tasks, timers, control actions, file access, or
  workspace mutation

## M4: External Event Surfaces V1

### Goal

Make `Holon` feel truly event-driven beyond local CLI input.

### Includes

- timer or cron-based wakeup
- one formal webhook adapter
- one remote-control style session ingress path
- preserve the rule that every external trigger becomes a queued event

### Definition Of Done

- a sleeping session can wake from timer events
- a sleeping session can wake from a real external HTTP trigger
- remote ingress behaves like a first-class session input, not a bypass path
- all event types remain auditable and carry provenance

### Explicitly Not Included

- full MCP channel system
- chat UI
- marketplace integrations

## M5: Multi-Session Runtime V1

### Goal

Turn the daemon from a single-session runtime into a minimal multi-session host.

### Includes

- session registry
- per-session queue and state isolation
- routing for status, enqueue, and brief access by session id
- session lifecycle controls
- basic cleanup and idle handling

### Definition Of Done

- multiple sessions can run under one process
- queue, state, and brief data stay isolated per session
- a single broken session does not destabilize others
- session routing is covered by integration tests

### Explicitly Not Included

- distributed cluster scheduling
- hosted control plane
- tenant management

## M6: Context Compaction And Long-Running Memory

### Goal

Support longer-running sessions without unbounded context growth.

### Includes

- add a first structured working-memory layer
- preserve key decisions, task identity, and trust metadata across summarization
- define an active context window versus durable working memory
- add deterministic compaction triggers

### Definition Of Done

- long sessions do not grow model context without bound
- essential decisions survive compression
- provenance and trust metadata remain recoverable
- compaction behavior is tested with long-session fixtures

### Current Status

- structured working memory now exists as a durable agent-state
  projection with revisioned prompt deltas
- durable episode memory now archives older work chunks as immutable records
  with an active per-session builder maintained at terminal turn boundaries
- the old flat `context_summary` remains only as a fallback during migration
- budget-aware prompt planning remains the main follow-on phase

### Explicitly Not Included

- full Claude Code-style multi-stage compact pipeline in the first iteration
- opaque heuristics without tests

## Current Recommendation

Near-term focus should stop at:

- M0
- M1
- M2

That sequence is enough to prove the central `Holon` thesis:

- a long-lived runtime
- a real context model
- background work that rejoins the same session loop

The project should not jump to multi-session or advanced compaction before
those three stages are stable.
