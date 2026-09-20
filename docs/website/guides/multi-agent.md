---
title: Multi-agent collaboration
summary: Creating and invoking agents, supervision contracts, and workspace modes for parallel work.
order: 35
---

# Multi-Agent Collaboration

> **Mental model & specification:** To understand how multi-agent actors fit into the broader architecture, see [Runtime model](/concepts/runtime-model.md). For supervisor-child lifecycle contracts and task handles, see [Tasks spec](/spec/tasks.md).

Holon supports creating addressable agents and invoking private supervised
agents for parallel work, delegation, and specialized subtasks.

## Overview of Collaboration Primitives

### Agent operations

| Tool / Operation | Return value | When to use |
|------------------|--------------|-------------|
| `CreateAgent` | `agent_id` | Create an independent, persistent, self-owned agent identity with an optional template, display name, and bootstrap message |
| `InvokeAgent` (new subagent) | `agent_id` + `task_handle` | Run a parent-supervised child task with result-bearing lifecycle and optional worktree isolation |
| `InvokeAgent` (existing agent) | `agent_id` + `task_handle` | Peer invocation: send a message to an existing authorized agent and wait for its next durable response |
| `SendAgentMessage` | delivery receipt | Send an asynchronous durable message to an existing authorized agent without creating a task wait handle |
| `GetAgent` | agent summary | Read agent-plane state (identity, display name, lifecycle, active work focus, waiting state, and child lineage) |

### Agent Identity & Display Names

- **Permanent Agent ID**: The canonical identifier (e.g. `reviewer`, `builder`) is permanent and cannot be changed.
- **Display Name**: Self-owned public agents can have a human-readable display name, updated via CLI (`holon agent rename <id> --name <name>`) or HTTP API (`PATCH /api/control/agents/:id/name`). The default agent cannot be renamed.
- **Incarnation**: A durable sequence tracking lifecycle resets and runtime reload generations for an agent.

### Workspace Modes

| Mode | Description |
|------|-------------|
| `inherit` (default) | Child shares the parent's workspace |
| `worktree` | Child gets an isolated worktree for safe experimentation |

### Task Handle Supervision

When calling `InvokeAgent`, the caller receives a `task_handle` with a `task_id`.

For **new subagents** (`kind: "new_subagent"`), the handle represents a supervised child task that produces a final completion result. Use this to:

- **TaskStatus** — Inspect lifecycle, waiting state, and metadata
- **TaskOutput** — Read bounded output or wait for completion
- **TaskInput** — Send follow-up input to the child
- **TaskStop** — Stop the child agent explicitly

For **existing agents** (`kind: "existing_agent"`), the handle waits for the first subsequent durable message emitted by the target agent. It satisfies the wait condition but is not a parent-child lifecycle containment boundary.

## Invocation Styles

### Supervised Child Subagent

Create a private subordinate agent strictly supervised by the current agent:

```json
{
  "target": {
    "kind": "new_subagent",
    "template": "code-reviewer",
    "workspace_mode": "worktree"
  },
  "initial_message": "Review pull request changes in src/runtime/"
}
```

### Peer Invocation

Invoke an existing long-lived agent as an equal peer:

```json
{
  "target": {
    "kind": "existing_agent",
    "agent_id": "auditor"
  },
  "initial_message": "Please audit runtime SQLite database retention rules."
}
```

### Child Agent Token Usage

Task status and output snapshots include a `token_usage` field with the
child agent's cumulative token consumption:

```json
{
  "total": {
    "input_tokens": 12450,
    "output_tokens": 3840,
    "total_tokens": 16290
  },
  "total_model_rounds": 5,
  "last_turn": {
    "input_tokens": 2100,
    "output_tokens": 720,
    "total_tokens": 2820
  }
}
```

| Field | Description |
|-------|-------------|
| `total` | Cumulative tokens across all child turns |
| `total_model_rounds` | Number of model round-trips the child has made |
| `last_turn` | Token usage for the most recent turn (if available) |

Use token usage to estimate child agent costs, detect unexpectedly expensive
delegations, or decide whether to stop a child that is consuming more tokens
than its output warrants.

## Usage Patterns

### Parallel investigation

Invoke multiple agents to explore different aspects simultaneously:

```
Parent agent:
  InvokeAgent("Review src/runtime/ for performance issues")
  InvokeAgent("Review src/runtime/ for error handling gaps")
  InvokeAgent("Review src/runtime/ for missing tests")
  → Wait for all task handles to complete
  → Aggregate findings into final report
```

### Specialized delegates

Assign specialized agents for distinct concerns:

```
Parent agent:
  InvokeAgent("Code review", template="code-reviewer")
  InvokeAgent("Test writing", template="test-writer")
```

### Safe experimentation

Use `worktree` mode to let a child experiment without affecting the main
workspace:

```
Parent agent:
  InvokeAgent("Try alternative implementation approach",
              workspace_mode=worktree)
  → Child works in isolated worktree
  → Parent reviews child's output
  → Parent applies the best approach to main workspace
```

## Supervision Flow

A typical parent-child interaction:

1. **Invoke** — Parent calls `InvokeAgent` with `initial_message` describing the
   task
2. **Monitor** — Parent uses `TaskStatus` to check if the child is still
   working, sleeping, or waiting
3. **Review** — Parent reads `TaskOutput` to get bounded previews or wait for
   completion
4. **Deliver** — Parent aggregates child results into the final user-facing
   answer

The parent remains responsible for:

- **Verification** — Child output is evidence, not authority
- **Aggregation** — Combining results from multiple children
- **Final delivery** — The parent produces the user-facing answer

## Best Practices

- **Keep delegations focused.** Each child should have one clear objective.
- **Supervise explicitly.** Check `TaskStatus` before assuming completion.
- **Treat child output as evidence.** Review and verify before passing to the
  user.
- **Limit parallelism.** Invoke only as many agents as the task actually
  benefits from.
- **Stop idle children.** Use `TaskStop` for children that are no longer
  needed.

## See Also

- [Runtime Model](/concepts/runtime-model.md) — Agent lifecycle and task
  supervision
- [Trust Boundaries](/concepts/trust-boundaries.md) — Why child output is
  evidence, not authority
- [Work Items Guide](/guides/work-items.md) — Tracking objectives across
  agents
