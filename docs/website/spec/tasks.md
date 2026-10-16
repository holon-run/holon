---
title: Tasks
summary: Current task lifecycle, terminal re-entry, and command/child-agent supervision contract.
order: 50
---

# Tasks

This page defines the current contract for managed task execution: lifecycle,
terminal re-entry, and supervision surfaces.

> **Last verified:** 2026-09-19 against `src/types.rs` `TaskRecord`,
> `TaskStatus`, `TaskKind`, `TaskHandle`, `TaskWaitPolicy`, and the tool
> implementations in `src/tool/tools/{exec_command,task_list,task_status,
> task_input,task_stop,invoke_agent}.rs`.

The output-observation contract was updated for #3534 M2; see
[Agent output observation](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-output-observation.md).

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
| `ChildAgentTask` | Parent-supervised child agent task |
| `ActorInvocation` | Canonical agent invocation task created by `InvokeAgent` |
| `SleepJob` | Internal sleep timer (not model-visible) |
| `SubagentTask` | Legacy child agent kind (migrating to `ChildAgentTask`) |
| `WorktreeSubagentTask` | Legacy worktree-isolated child agent (migrating) |

## Child lifecycle disposition

Parent-supervised task recovery records persist an
`AgentLifecycleDisposition`:

| Disposition | Terminal behavior |
|-------------|-------------------|
| `retain` | Persist the terminal task/result without deleting the child |
| `delete_on_terminal` | Atomically persist the terminal task/result and admit the canonical deletion job |

`ActorInvocation`, including `InvokeAgent(new_subagent)`, defaults to `retain`
so the created actor remains reusable. Legacy and current `ChildAgentTask`
records default to `delete_on_terminal`; deletion is admitted only for the
resolved private, parent-supervised, ephemeral child owned by that task.

## Task lifecycle

```text
              ExecCommand / InvokeAgent
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
  resource=<task_id>, delivery=...)` for the terminal `TaskResult`, not status or
  output polling. Choose `delivery=final` or `delivery=silent` explicitly.
- Result-bearing terminal `TaskResult` events re-enter the agent as continuation
  context when they hold the matching wait or independent rejoin authority.
- `InvokeAgent(existing_agent)` completes its observation handle when the first
  accepted later message from that sender arrives. Its TaskResult contains
  message/delivery references, not a copy of the reply or business completion.
  The original message carries the content and resumes an exact task wait once.
  A late `WaitFor` uses that same message; an already consumed reply continues
  the current turn. Review the original delivered message and its references.
- The former model tool `TaskOutput` and CLI `holon task output` are removed.
  Their `block`/`timeout_ms` current-turn short wait is no longer provided.
  Bounded `holon tail --agent` follow is observation, not a completion wait.

## Supervision tools

| Tool | Purpose |
|------|---------|
| `ListTasks` | Compact active-task digest (non-terminal tasks only) with bounded output |
| `TaskStatus` | Single-task lifecycle snapshot with metadata |
| `TaskInput` | Send stdin/follow-up input to an interactive task |
| `TaskStop` | Stop a running task (may transition through `Cancelling`) |

**Key contract:**

- `ListTasks` excludes terminal tasks; use `TaskStatus` for historical detail.
- Inspect command output by reading only runtime-confirmed files in bounded
  byte ranges. Do not construct a path from an agent home or task id. An absent
  file is explicit unavailability, not permission to guess another path.
- Use bounded `holon tail --agent <agent_id> --include-progress` observation
  for assistant progress and briefs. It is agent-scoped, not task-exclusive;
  it does not consume results, register a task waiter, or wake the producer.
- `TaskInput` accepts input only for tasks created with interactive
  continuation enabled (`accepts_input=true`).
- `TaskStop` sends a stop request; the task may first enter `Cancelling`
  before reaching `Cancelled`.

### Output observation is not completion

For a command file path confirmed by the runtime, a bounded read can use:

```bash
dd if="$CONFIRMED_OUTPUT_PATH" bs=1 skip=0 count=8192 status=none
```

Set `CONFIRMED_OUTPUT_PATH` to the exact returned host path, not an inferred
location. Capture may retain only a head/tail and rewrite the artifact at
termination; a running file is not a lossless append-only stream. Wait for
termination before relying on a stable final log. File EOF, tail EOF, an `end`
receipt, or an idle agent does not establish task completion.

The underlying HTTP task-output route and runtime capture/result services remain
available; only the model tool and CLI command are retired. See the
[CLI observation reference](../reference/cli.md#agent-output-observation).

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
  is now expressed with `WaitFor(wake=task_result, resource=<task_id>,
  delivery=...)` plus terminal task re-entry. The former model/CLI synchronous
  output short wait was removed in #3534 M2.
