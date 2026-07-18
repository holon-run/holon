# Agent Workspace Tool Lifecycle

## Choice

Expose binding, activation, and worktree artifact lifecycle as separate
runtime transitions, then compose only the common one-call workflows:

- `CreateWorktree` creates or reuses and activates by default;
- active `DetachWorkspace` switches to `agent_home` before detaching;
- active `RemoveWorktree` switches to an explicit `return_to` target before
  clean-only removal.

Keep `UseWorkspace` hidden but dispatch-compatible for existing transcripts and
providers.

## Reason

Runtime database usage showed that agents prefer one-call create-and-enter and
exit-and-clean flows, while implicit creation inside a generic switch operation
makes retries and conflicts unsafe. The split keeps state transitions and audit
evidence explicit without forcing the model to manually chain every common
operation.

## Preserved Boundary

`SwitchWorkspace` never attaches, creates, or removes resources.
`DetachWorkspace` never removes worktrees or branches. `RemoveWorktree` never
accepts an arbitrary path or force flag.
