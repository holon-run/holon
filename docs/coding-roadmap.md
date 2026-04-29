# Holon Coding Roadmap

This document defines the roadmap for turning `Holon` from a general long-lived
runtime into a coding-capable agent runtime.

The design reference is primarily the Claude Code runtime shape:

- query loop
- tool use / tool result
- task and subagent orchestration
- context management and compaction
- policy and sandbox boundaries
- explicit user-facing output

The goal is not to clone Claude Code feature-for-feature. The goal is to reach
the point where `Holon` can reliably complete coding-oriented tasks.

## Status

The first full pass of `R1` through `R8` is now implemented in the repository.

Current implementation highlights:

- Claude-style `tool_use` / `tool_result` runtime loop
- shell-first repo inspection plus local file mutation tools
- host-owned workspace registry plus explicit agent workspace attachment
- explicit `workspace_anchor`, `active_workspace_entry`, `execution_root`, and
  `cwd` binding
- explicit workspace projection and single-writer occupancy semantics
- runtime-derived closure outcome and waiting-reason surfaces
- typed continuation resolution with blocking `TaskResult` rejoin
- explicit objective state with `current_objective`, `last_delta`, and
  `acceptance_boundary`
- coding-oriented runtime prompt and explicit `Sleep` handling
- background task and bounded delegated child execution
- recent message / brief / tool-result context plus deterministic compaction
- explicit origin/trust/delivery-surface/admission-context marking
- trust-aware tool exposure with final restriction policy still deferred
- explicit `host_local` execution-policy snapshots and honest non-sandbox
  capability reporting
- explicit agent-level model override with effective-model inspection
- unit, integration, live-provider, and fixture-based regression tests
- a thin local TUI for driving and observing the long-lived runtime while
  dogfooding coding flows
- a chat-first TUI interaction model with direct composer input and overlay
  views for agents, transcript, and tasks
- an explicit `holon daemon` lifecycle surface for starting, stopping, and
  inspecting the local long-lived runtime
- concise daemon activity reporting that distinguishes healthy-idle,
  healthy-waiting, and healthy-processing runtime states
- daemon-status visibility into the most recent runtime failure summary
- one documented local troubleshooting path that tells operators when to use
  `run`, `daemon status`, `daemon logs`, `status` / `transcript`, `tui`, and
  foreground `serve`
- provider attempt timelines that preserve retry, fail-fast, and fallback
  behavior on transcript and audit surfaces
- structured token usage on `run` / `status` plus per-turn token metadata on
  transcript and audit surfaces

This does not mean the coding runtime is “finished”. It means the first
end-to-end version of the roadmap now exists in code and has automated
verification.

## Coding Goal

`Holon` should eventually be able to:

- accept a coding task
- inspect a workspace
- edit files
- run commands
- observe failures
- inspect daemon lifecycle failures through a local-first CLI surface
- retry or refine changes
- report progress and final results clearly
- split work into subtasks when needed

## R1: Tool Runtime

### Goal

Implement a Claude Code-style tool runtime based on `tool_use` and
`tool_result`.

### Includes

- define a `Tool` trait
- add a tool registry and schema surface
- teach the provider/runtime loop to handle:
  - model response
  - tool invocation
  - tool execution
  - tool result re-entry
- preserve tool executions in the audit log and active context

### Definition Of Done

- the runtime can complete a loop involving at least one real tool call
- tool results re-enter the same session loop
- `brief` output still stays separate from internal tool traces

### Implemented Notes

- `AnthropicProvider` now supports native tool schemas and `tool_use` blocks
- `RuntimeHandle` runs an iterative tool loop with `tool_result` re-entry
- tool executions are persisted to `tools.jsonl` and included in active context

### Initial Tool Set

- `Sleep`
- `GetSessionState`
- `Enqueue`
- managed task inspection/control only; no public `CreateTask`

## R2: Workspace Base Tools

### Goal

Add the minimum file and workspace tools needed for coding tasks.

### Includes

- `ListFiles`
- `SearchText`
- `ReadFile`
- `ApplyPatch`

### Definition Of Done

- the agent can inspect a codebase
- the agent can make targeted edits to files
- workspace changes are visible in subsequent turns

### Notes

This stage should avoid shell access. The goal is to first prove the coding loop
through file tools alone.

### Implemented Notes

- `ListFiles`, `SearchText`, `ReadFile`, and `ApplyPatch` are live
- file tools are scoped to the configured workspace root
- workspace mutations are visible to later turns and regression tests
- the normal model-facing inspection surface now retires repo-native
  `Read` / `Glob` / `Grep` in favor of shell-first `exec_command`

## R3: Command Execution And Safety

### Goal

Enable real engineering tasks that require commands and validation.

### Includes

- `ExecCommand`
- `KillCommand`
- stdout/stderr capture
- command result persistence and context injection
- policy gating for shell access

### Definition Of Done

- the agent can run tests or build commands
- command output becomes available to later reasoning
- command execution is policy-controlled and auditable

### Notes

Policy should ship before or alongside shell access. Shell should not arrive as
an unrestricted convenience feature.

### Implemented Notes

- `ExecCommand` and `KillCommand` are live
- command output is persisted through tool execution records and fed back into
  later turns
- shell tools are only exposed to trusted operator/system inputs

## R4: Coding Task Loop

### Goal

Make the runtime behave like a coding agent rather than a generic event-driven
assistant.

### Includes

- coding-focused runtime prompt
- plan / act / verify loop shape
- explicit progress and final-result `brief` behavior
- working-set awareness for files and recent tool results

### Definition Of Done

- the agent can complete a small bugfix task end-to-end
- it can inspect code, edit it, run validation, and summarize the result

### Implemented Notes

- the runtime prompt is explicitly coding-oriented
- `Sleep` is treated as a first-class runtime termination signal
- integration tests now cover file-edit and shell-validation loops

## R5: Task And Subagent Runtime

### Goal

Upgrade `Task` into a real orchestration primitive for coding work.

### Includes

- task kinds beyond the current background placeholder jobs
- `ChildAgentTask`
- context handoff into subagent work
- result and status aggregation back to the parent session

### Definition Of Done

- the runtime can delegate bounded coding subtasks
- the parent session can continue while background subtasks run
- task results rejoin the main queue without losing provenance

### Notes

This should follow the Claude Code pattern where subagents reuse the same core
runtime rather than becoming a separate architecture.

### Implemented Notes

- short session-local waiting now belongs to `Sleep(duration_ms)`
- managed command execution is created publicly through `exec_command`
- bounded child-agent creation now belongs to `SpawnAgent`
- parent-supervised child execution still rejoins through task/result surfaces
- parent supervision now uses a structured child observability snapshot across
  `TaskStatus` and `AgentGet.active_children` instead of relying on raw output
  polling for in-flight delegated progress

## R6: Coding Context Management

### Goal

Make long-running coding sessions hold onto the right information.

### Includes

- recent message window
- recent tool result window
- working-set file references
- deterministic summary / compaction
- explicit separation between durable history and model-visible context

### Definition Of Done

- the agent does not immediately forget prior edits or command results
- important file changes and decisions survive longer coding sessions
- context growth remains bounded

### Implemented Notes

- model-visible context now includes recent messages, briefs, and tool
  executions
- structured working memory is now derived from durable runtime
  evidence and rendered ahead of volatile turn context
- durable episode memory now accumulates terminal-turn evidence into one active
  builder and freezes immutable archived work chunks on semantic boundaries
- prompt-visible working-memory deltas are emitted on the next turn after a
  durable revision change
- deterministic compaction still keeps a bounded legacy summary of older
  messages as a fallback path
- the latest completed result is surfaced explicitly in context for follow-up
  questions

## R7: Coding Trust Model

### Goal

Define a trust and permission model suitable for coding agents.

### Includes

- separate permission boundaries for:
  - file tools
  - shell tools
  - task/subagent tools
- operator versus external ingress distinctions
- stable provenance and admission marks on coding turns
- policy hooks for sensitive actions
- optional workspace/path scoping

### Definition Of Done

- external triggers do not silently gain coding authority
- file and shell actions are inspectable and policy-gated
- the runtime can explain both how a trigger was admitted and why an action was
  or was not allowed

### Implemented Notes

- tool exposure is trust-aware
- policy gates remain in place for task creation, timers, control actions, and
  origin/kind compatibility
- file path resolution rejects writes outside the configured workspace root

## R8: Test And Verification Matrix

### Goal

Make coding behavior durable by building a full verification layer.

### Includes

- unit tests for:
  - tool schema and dispatch
  - context building
  - policy decisions
  - task lifecycle
  - compaction behavior
- integration tests for:
  - file edit loops
  - shell execution loops
  - multi-session isolation
  - remote ingress
  - task/subagent rejoin
- live provider tests using real provider configuration
- fixture-based regression cases for longer coding tasks

### Definition Of Done

- each major coding subsystem has direct unit coverage
- each major runtime behavior has integration coverage
- live provider tests continue to pass when run explicitly
- regressions in context, tool use, or policy are detectable before release

### Implemented Notes

- unit tests cover queue, config, policy, storage, context compaction, and
  runtime basics
- integration tests cover tool loops, file edits, shell execution, task rejoin,
  remote ingress, and multi-session isolation
- live provider tests exist for Anthropic, OpenAI, and OpenAI Codex and run
  against local real configuration when invoked explicitly
- fixture-based regression coverage now exists under `tests/fixtures/`

## Recommended Order

The recommended order remains:

1. `R1`
2. `R2`
3. `R3`
4. `R4`
5. `R5`
6. `R6`
7. `R7`
8. `R8`

That order keeps the project honest:

- first build the execution model
- then add coding primitives
- then add delegation and memory
- finally harden with policy and tests
