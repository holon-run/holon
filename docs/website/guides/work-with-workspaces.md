---
title: Work in a project workspace or worktree
summary: Attach a project, activate its execution root, isolate changes in a managed worktree, and clean up safely.
order: 60
---

# Work in a project workspace or worktree

Use workspace activation to select where runtime tools work. A shell `cd`
changes only that command's directory. See
[Workspaces](/concepts/workspaces.md) for bindings and execution roots.

## Before you start

- An existing agent and an accessible local project.
- A Git repository if you need a linked worktree.
- Permission to edit that project; a workspace is not a filesystem sandbox.
- A cleanly identified base ref and a new branch name for isolated work.

The shell commands below run in your terminal. JSON examples are agent tool
calls: have the selected agent issue them through its exposed tools. There is
no `holon workspace switch` CLI command.

## Steps

1. Attach the project to your agent, replacing both values:

   ```bash
   holon workspace attach --agent developer /absolute/path/to/project
   ```

   Attach adds a binding without activating it. Record the returned workspace ID.

2. Have the agent inspect bindings with `GetWorkspaceState({})`, then activate
   the returned workspace ID:

   ```text
   SwitchWorkspace({"workspace_id": "ws_returned_by_attach"})
   ```

   Call `GetWorkspaceState({})` again. Confirm the active root and default cwd
   identify the project before making any edit.

3. Inspect the repository before choosing a base:

   ```bash
   git status --short
   git branch --show-current
   git rev-parse --verify HEAD
   ```

   Run these in the activated root. Preserve unrelated changes. For an isolated
   edit, ask the agent to create a managed worktree:

   ```text
   CreateWorktree({
     "workspace_id": "ws_returned_by_attach",
     "branch": "docs/example-update",
     "base_ref": "HEAD",
     "activate": true,
     "on_existing": "error"
   })
   ```

   `HEAD` means the inspected repository base; use a specific verified commit
   or ref when you need a different starting point. Keep the returned execution
   root ID and path. Do not infer the worktree's location from its branch name.

4. Confirm the active worktree with `GetWorkspaceState({})`, then run `pwd`,
   `git branch --show-current`, and `git status --short` there. Make only the
   authorized changes and run the project's relevant checks.

## Confirm it worked

The runtime state, `pwd`, and Git branch agree on the intended worktree.
`git diff --stat` shows your edits there. Inspect the canonical checkout
separately to confirm you did not put the edits in the wrong root.

## Return and clean up

Preserve the changes through your normal review/commit process before cleanup.
Do not remove a worktree that another execution still uses.

```text
SwitchWorkspace({"workspace_id": "ws_returned_by_attach"})
RemoveWorktree({
  "execution_root_id": "returned_worktree_root_id",
  "branch_policy": "keep"
})
```

This keeps the branch. Removal requires a clean, registered, unoccupied
worktree. To remove an active worktree, `RemoveWorktree` also requires a valid
`return_to` selector; switching out first makes the destination explicit.

Return to agent home or detach the binding when no longer needed:

```bash
holon workspace exit --agent developer
holon workspace detach --agent developer ws_returned_by_attach
```

Detaching removes the agent's binding, not the project files.

## Troubleshooting

- **Attached but commands still run at home:** activation is a separate step.
- **Existing branch/worktree conflict:** inspect registered roots. Use
  `on_existing: "reuse"` only when the existing worktree is the intended one;
  reuse does not reset it to your requested base.
- **Cleanup refused:** inspect dirty files, occupancy, locks, and registration.
  Resolve the reported condition instead of forcing deletion.
- **Tool unavailable:** check the agent's capability policy. Do not replace a
  denied lifecycle operation with manual metadata edits or `rm -rf`.

Exact selectors and lifecycle rules are in the
[Workspace reference](/reference/workspaces.md).
