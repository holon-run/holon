---
title: Delegate work to another agent
summary: Hand a scoped task to a child agent, wait for the result, and handle what comes back.
order: 13
---

# Delegate work to another agent

When a piece of work is separable, hand it to another agent and keep going. This
guide covers choosing how to invoke that agent, waiting for the result, and
handling what comes back.

Delegation has a model behind it: who may act, how far authority reaches, and
how results are trusted. See [Multi-agent collaboration](/concepts/multi-agent-collaboration.md).

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

- [Multi-agent collaboration](/concepts/multi-agent-collaboration.md) — roles,
  boundaries, and trust
- [Trust boundaries](/concepts/trust-boundaries.md) — why child output is
  evidence, not authority
- [Work items reference](/reference/work-items.md) — tracking objectives across
  agents
