---
title: Delegate work to another agent
summary: Choose a child or peer, follow its reply, verify the result, and clean up owned children.
order: 80
---

# Delegate work to another agent

Delegate a bounded piece of work that you can verify independently, such as a
read-only review of a diff. The parent still owns acceptance and delivery to the
operator.

## Before you start

Give the parent access to the intended workspace and a clear objective. Confirm
that `InvokeAgent` is available. A template named in an invocation must already
be available; an existing peer must exist and be accessible to the caller.

The JSON blocks below are **agent tool inputs**, not shell commands. As an
operator, ask the parent to perform these steps. Do not start recursive
`holon run` commands to imitate delegation.

## Invocation Styles

### Supervised Child Subagent

Use a child for a separable task under the parent's supervision. Call
`InvokeAgent` with scope, restrictions, and a return format:

```json
{
  "target": {
    "kind": "new_subagent",
    "template": "code-reviewer",
    "workspace_mode": "inherit"
  },
  "initial_message": "Review the current diff in src/runtime/. Read only; do not edit, commit, or publish. Return findings with file locations, severity, and verification gaps. If no findings, say what you checked."
}
```

Save both the returned `agent_id` and task handle. This read-only example
inherits the workspace so the child can inspect the current uncommitted diff.
Keep that diff stable during review.

For an editing assignment, use `workspace_mode: "worktree"` and identify the
committed revision or patch the child must inspect. A fresh worktree does not
implicitly contain the parent's uncommitted changes. A worktree separates
repository edits; it is not a security sandbox. Specify which files the child
may change and whether it should commit.

### Peer Invocation

Use a peer when an existing agent owns the expertise or ongoing responsibility:

```json
{
  "target": { "kind": "existing_agent", "agent_id": "auditor" },
  "initial_message": "Audit the current retention change without editing. Reply with findings and the commit or diff you inspected; distinguish an acknowledgement from a completed audit."
}
```

This leaves the peer's configuration and parentage unchanged. Its handle resolves
on the **first later durable message**, which may only acknowledge the request.
Check that the reply actually satisfies the assignment. Concurrent waits can see
the same reply; they do not each imply a separate completed task.

### Child Agent Token Usage

Use task snapshots to inspect `token_usage` when investigating cost or unexpected
work. The field definitions are in the
[delegated task reference](../reference/work-items.md#delegated-task-snapshots).
Token consumption does not establish progress or correctness.

## Supervision Flow

1. **Invoke** — Save the task handle returned by `InvokeAgent`.
2. **Monitor** — Use `TaskStatus` for task lifecycle and bounded `holon tail --agent <child_id> --include-progress` for agent progress. Tail is agent-scoped and non-exclusive; observation or EOF does not mean the task completed.
3. **Wait** — When only completion remains, call `WaitFor` with the returned task ID:

   ```json
   {
     "wake": "task_result",
     "resource": "TASK_ID_FROM_INVOKE_AGENT",
     "reason": "Wait for the scoped review",
     "delivery": "silent"
   }
   ```

4. **Review on re-entry** — Follow `result.continuation`. `continue_turn` means use the available result now; a yielding result means resume when delivered. For peer invocations, inspect the delivered message referenced by the result.
5. **Verify** — Check the delegate's evidence against the assigned scope. Child output is evidence, not authority.
6. **Deliver** — Produce the combined answer with verification status and retained artifact locations.

For follow-up input, inspect task status first and use `TaskInput` only when supported. A terminal task cannot be restarted with input; invoke the retained agent again or send a durable message.

### Subagent Lifecycle and Cleanup

Newly invoked subagents are private, reusable instances retained after their tasks complete. They do not expire automatically by age:

- **Parent ownership:** Only the supervising parent can delete a retained child. The runtime validates canonical supervision and agent incarnations.
- **Cleanup inspection:** Calling `GetAgent` on the parent surfaces a bounded `subagent_cleanup` list (up to 16 supervised children) with any blockers.
- **Blocker checks:** Active execution, unresolved descendant responsibilities, and dirty or shared worktrees block cleanup until settled. Open work items or completed tasks do not block cleanup if the parent decides they are no longer needed.
- **Delete when done:** Call `DeleteAgent(agent_id, incarnation)` once child artifacts are incorporated and blockers cleared.
- **Background reminders and retirement:** The daemon can send deduplicated daily reminders to parents with lingering children (`reminders_enabled`). If configured, fully idle child runtimes can cooperatively exit memory (`idle_retirement_enabled`) while preserving durable identity and history on disk.

## Usage Patterns

### Parallel investigation

Assign non-overlapping questions and retain each handle. Verify and reconcile
conflicting findings before reporting them. Limit parallelism to work that can
actually proceed independently.

### Specialized delegates

Choose an installed template for the task, but include acceptance criteria in
the assignment. A role name alone does not tell the child what to deliver.

### Safe experimentation

For an authorized experiment, use `worktree` mode and request a diff and test
results. Inspect that worktree before integrating changes; do not assume they
already exist in the parent's checkout.

## Best Practices

If invocation is rejected, check the target ID, template availability, and caller
permissions. Do not create a same-named replacement or broaden authority to work
around the rejection. If a child waits for input, answer the concrete question
or stop the task when its work is no longer needed.

Success means the parent has a verified result, can locate its artifacts, and
has either retained the child intentionally or completed safe cleanup.

## See Also

- [Multi-agent collaboration](../concepts/multi-agent-collaboration.md)
- [Tasks specification](../spec/tasks.md)
- [Observe agent work](observe-agent-work.md)
