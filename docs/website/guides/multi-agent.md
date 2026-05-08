# Multi-Agent Collaboration

Holon supports spawning child agents for parallel work, delegation, and
specialized subtasks. Each child agent runs in its own context with a
well-defined supervision contract.

## Concepts

### Agent Presets

| Preset | Ownership | Return value | When to use |
|--------|-----------|-------------|-------------|
| `private_child` (default) | Parent-supervised | `agent_id` + `task_handle` | Delegated subtasks, parallel work |
| `public_named` | Self-owned | `agent_id` only | Long-lived, addressable agents |

### Workspace Modes

| Mode | Description |
|------|-------------|
| `inherit` (default) | Child shares the parent's workspace |
| `worktree` | Child gets an isolated worktree for safe experimentation |

### Task Handle Supervision

When spawning a `private_child`, the parent receives a `task_handle` with a
`task_id`. Use this to:

- **TaskStatus** — Inspect lifecycle, waiting state, and metadata
- **TaskOutput** — Read bounded output or wait for completion
- **TaskInput** — Send follow-up input to the child
- **TaskStop** — Stop the child agent explicitly

## Usage Patterns

### Parallel investigation

Spawn multiple children to explore different aspects simultaneously:

```
Parent agent:
  SpawnAgent("Review src/runtime/ for performance issues")
  SpawnAgent("Review src/runtime/ for error handling gaps")
  SpawnAgent("Review src/runtime/ for missing tests")
  → Wait for all task handles to complete
  → Aggregate findings into final report
```

### Specialized delegates

Assign specialized agents for distinct concerns:

```
Parent agent:
  SpawnAgent("Code review", template="code-reviewer")
  SpawnAgent("Test writing", template="test-writer")
```

### Safe experimentation

Use `worktree` mode to let a child experiment without affecting the main
workspace:

```
Parent agent:
  SpawnAgent("Try alternative implementation approach",
             workspace_mode=worktree)
  → Child works in isolated worktree
  → Parent reviews child's output
  → Parent applies the best approach to main workspace
```

## Supervision Flow

A typical parent-child interaction:

1. **Spawn** — Parent calls `SpawnAgent` with `initial_message` describing the
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
- **Limit parallelism.** Spawn only as many children as the task actually
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
