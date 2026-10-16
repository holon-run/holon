---
title: Run a long-lived task
summary: Track one objective across sessions, wait for dependencies, and verify its completion report.
order: 70
---

# Run a long-lived task

Use a WorkItem when work needs to continue across sessions or wait for a decision,
CI run, or another task. This walkthrough keeps one objective from planning to
acceptance. A durable record lets the agent recover its goal; it does not guarantee
that a shell process survives a daemon crash or machine reboot.

## Before you start

Have a configured model, an existing agent, and access to the project it will
work on. Run `holon config doctor` if model setup is incomplete. The shell commands
below are operator commands; JSON examples are tool calls made by the agent.

## The workflow end-to-end

### 1. Start the daemon

```bash
holon daemon start
holon daemon status
```

Continue when the daemon reports that it is running. See
[Troubleshooting](troubleshooting.md#daemon-issues) if startup fails.

### 2. Create a specialized agent

If you already have a suitable agent, reuse it. Otherwise, with the
`software-developer` template installed:

```bash
holon agent create builder --template software-developer
holon agent status builder
```

Confirm that `builder` has the intended workspace before authorizing edits.
Creating an agent alone does not select a repository for your task.

### 3. Start a WorkItem-backed piece of work

Submit an explicit objective and acceptance boundary to that agent:

```bash
holon prompt --agent builder \
  "Track one WorkItem to improve error handling in the attached project. Inspect the code and write a plan with acceptance checks. Ask me to approve the plan before editing. Do not push or open a PR."
```

The agent checks for an existing item before creating another. It records scope,
acceptance checks, and unresolved decisions in the plan, and progress in the todo
list. Review the plan location returned by the runtime. Approve it only after the
scope is clear:

```bash
holon prompt --agent builder \
  "The plan is approved. Continue the same WorkItem, make the scoped edits, and run its acceptance checks."
```

The agent changes `plan_status` to `ready` and continues that item. Approving a
plan is not completion of the objective.

### 4. Check progress from another session

```bash
holon agent status builder
holon work-item list --agent builder
holon work-item get WORK_ITEM_ID --agent builder
holon tail --agent builder --limit 10
```

Replace `WORK_ITEM_ID` with the ID returned by the list. Match the objective,
plan, and checklist before sending more instructions. A current item is the
agent's focus; it may still be waiting or yielded to another item.

### 5. Wait for external events

#### Waiting for commands to finish

Short commands may finish immediately. When a command becomes a background task,
the agent saves its returned task ID and waits with `WaitFor`:

```json
{
  "wake": "task_result",
  "resource": "TASK_ID_FROM_EXEC_COMMAND",
  "reason": "Wait for the acceptance test result",
  "delivery": "silent"
}
```

The agent follows the returned `continuation`: it may continue immediately if the
result is already available. Task ownership stays with the WorkItem that created
it; changing focus or waiting from another item does not move the task.

#### Waiting for operator input

For a decision, the agent records the question, sets `plan_status=needs_input`
when appropriate, and calls `WaitFor(wake=operator_input)` with a reason and
explicit delivery mode. You can reply later with `holon prompt --agent builder`.
The agent rechecks the decision and updates the same plan before resuming.

#### Waiting for external triggers

For CI or a review, the agent records the outside object and registers a wait:

```json
{
  "wake": "external",
  "resource": "github:owner/repo#123",
  "reason": "Wait for CI before evaluating the change",
  "recheck_after_ms": 600000,
  "delivery": "final"
}
```

With `delivery=final`, a yielding path requires a non-empty waiting report.
`WaitFor` does not install a webhook. Follow
[Connect external events](connect-external-events.md) to configure delivery.
After a wake, the agent reads CI's actual state before acting. A wake does not
prove that the dependency passed or authorize a merge.

### 6. Handle interruptions

| Interruption | Recovery check |
|---|---|
| Terminal or TUI disconnect | Reconnect and inspect the same agent and WorkItem. The daemon may continue working. |
| Daemon restart | Read the persisted plan, readiness, waits, and task state before resuming. |
| Machine reboot | Start the daemon, then check which commands need rerunning. Do not assume a prior process survived. |
| Lost callback or stale CI state | Recheck the external source; inspect the wait and callback configuration before retrying. |

Do not recreate the objective just because a client disconnected. If an item is
`yielded`, inspect the item it yielded to; do not manually clear a blocker to
force a return.

### 7. Receive the final brief

After acceptance checks pass, the agent calls `CompleteWorkItem` and supplies a
non-empty completion report. Retrieve the item again and confirm:

- its state is `completed`;
- `result_brief_id` identifies its report;
- the report names the changes, verification outcome, and any remaining limits.

Inspect the actual diff or output files as well as the report. A successful
background command is only one piece of evidence for the objective.

## When to use durable workflows

Use this workflow for tracked multi-session changes and external dependencies.
For a short question or inspection that can finish in one turn, ask directly
without creating a WorkItem. For recurring work, see
[Schedule agent work](schedule-agent-work.md).

## See also

- [Work items reference](../reference/work-items.md)
- [Observe agent work](observe-agent-work.md)
- [Delegate work](delegate-work.md)
- [Context continuity](../concepts/context-continuity.md)
