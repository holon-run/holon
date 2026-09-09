---
title: Multi-agent collaboration
summary: Creating and invoking agents, supervision contracts, and workspace modes for parallel work.
order: 35
---

# Multi-Agent Collaboration

Holon supports creating addressable agents and invoking private supervised
agents for parallel work, delegation, and specialized subtasks.

## Concepts

### Agent operations

| Operation | Return value | When to use |
|-----------|--------------|-------------|
| `CreateAgent` | `agent_id` | Create a long-lived, addressable agent |
| `InvokeAgent` | `agent_id` + `task_handle` | Run a parent-supervised delegated task |

### Workspace Modes

| Mode | Description |
|------|-------------|
| `inherit` (default) | Child shares the parent's workspace |
| `worktree` | Child gets an isolated worktree for safe experimentation |

### Task Handle Supervision

When calling `InvokeAgent`, the parent receives a `task_handle` with a `task_id`.
Use this to:

- **TaskStatus** — Inspect lifecycle, waiting state, and metadata
- **TaskOutput** — Read bounded output or wait for completion
- **TaskInput** — Send follow-up input to the child
- **TaskStop** — Stop the child agent explicitly

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
