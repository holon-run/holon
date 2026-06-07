---
title: Tasks
summary: Current task lifecycle, terminal re-entry, and command/child-agent supervision contract.
order: 50
---

# Tasks

This page defines the current contract for managed task execution: lifecycle,
terminal re-entry, and supervision surfaces.

> **Last verified:** 2026-05-25 against `src/types.rs` `TaskRecord`,
> `TaskStatus`, `TaskKind`, `TaskHandle`, `TaskWaitPolicy`, and the tool
> implementations in `src/tool/tools/{exec_command,task_list,task_status,
> task_output,task_input,task_stop,spawn_agent}.rs`.

## Source RFCs

- [Command Tool Family](https://github.com/holon-run/holon/blob/main/docs/rfcs/command-tool-family.md)
- [Task Surface Narrowing](https://github.com/holon-run/holon/blob/main/docs/rfcs/task-surface-narrowing.md)
- [Interactive Command Continuation](https://github.com/holon-run/holon/blob/main/docs/rfcs/interactive-command-continuation.md)
- [Agent Delegation Tool Plane](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-delegation-tool-plane.md)
- [Agent Control Plane Model](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-control-plane-model.md)
- [Runtime Scheduler Contract](https://github.com/holon-run/holon/blob/main/docs/rfcs/runtime-scheduler-contract.md)

## Task kinds

| `TaskKind` | Description |
|------------|-------------|
| `CommandTask` | Shell command execution via `ExecCommand` |
| `ChildAgentTask` | Parent-supervised delegated child agent via `SpawnAgent` |
| `SleepJob` | Internal sleep timer (not model-visible) |
| `SubagentTask` | Legacy child agent kind (migrating to `ChildAgentTask`) |
| `WorktreeSubagentTask` | Legacy worktree-isolated child agent (migrating) |

## Task lifecycle

```text
              ExecCommand / SpawnAgent
                       │
                       ▼
                 ┌──────────┐
                 │  Queued  │
                 └────┬─────┘
                      │
                      ▼
                 ┌──────────┐     TaskStop
                 │ Running  │──────────────┐
                 └────┬─────┘              │
                      │                    ▼
          ┌───────────┼───────────┐  ┌────────────┐
          ▼           ▼           ▼  │ Cancelling │
    ┌──────────┐ ┌──────────┐ ┌────┴─┴─────┐      │
    │Completed │ │  Failed  │ │Interrupted│      ▼
    └──────────┘ └──────────┘ └───────────┘┌──────────┐
                                           │Cancelled │
                                           └──────────┘
```

**Terminal states:** `Completed`, `Failed`, `Cancelled`, `Interrupted`.
**Non-terminal states:** `Queued`, `Running`, `Cancelling`.

## Wait policy

Each task carries a wait policy for task-list and task-status compatibility:

| `TaskWaitPolicy` | Behavior |
|------------------|----------|
| `Background` | Task runs independently; agent can continue turns while task is active |

All current task kinds report `Background`. Historical task detail payloads may
still contain `wait_policy: "blocking"`, but the runtime ignores that value for
scheduler blocking decisions.

**Key contract:**

- For background tasks, use `WaitFor(wake=task_result,
  resource=<task_id>)` to wait for the terminal `TaskResult` instead of
  polling `TaskOutput`.
- The terminal `TaskResult` event re-enters the agent as continuation context;
  the runtime wakes the agent automatically.
- `TaskOutput(block=true)` is for explicit current-turn synchronous waiting,
  not the default waiting strategy.

## Supervision tools

| Tool | Purpose |
|------|---------|
| `ListTasks` | Compact active-task digest (non-terminal tasks only) with bounded output |
| `TaskStatus` | Single-task lifecycle snapshot with metadata |
| `TaskOutput` | Bounded output preview with optional `block=true` |
| `TaskInput` | Send stdin/follow-up input to an interactive task |
| `TaskStop` | Stop a running task (may transition through `Cancelling`) |

**Key contract:**

- `ListTasks` excludes terminal tasks; use `TaskStatus` for historical detail.
- `TaskOutput` returns a bounded `output_preview` plus artifact refs for full
  output; it is for inspection, not polling.
- `TaskInput` accepts input only for tasks created with interactive
  continuation enabled (`accepts_input=true`).
- `TaskStop` sends a stop request; the task may first enter `Cancelling`
  before reaching `Cancelled`.

## Distinction from WorkItems and waiting

Tasks are **execution handles**, not planning objects:

- A `Task` represents a running or queued execution unit.
- A `WorkItem` represents a durable objective the agent is working toward.
- `Waiting` represents why a WorkItem or agent cannot proceed.

Tasks often serve WorkItem objectives (running commands, delegating to child
agents), but task lifecycle is independent of WorkItem lifecycle.

## Resolved gaps

- [Issue #1382](https://github.com/holon-run/holon/issues/1382) removed the
  unused `Blocking` task wait policy from the public/runtime contract. Waiting
  is expressed with `WaitFor(wake=task_result)` plus terminal task re-entry, or
  with a bounded `TaskOutput(block=true)` call inside the current turn.
