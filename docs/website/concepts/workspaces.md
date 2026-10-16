---
title: Workspaces and working directories
summary: Agent Home, project workspaces, execution roots, and worktrees, with the limits of workspace isolation.
order: 50
---

# Workspaces and working directories

A workspace gives an agent a project context: where scoped guidance applies
and where tools work by default. Every agent has one active workspace at a
time. It can have other workspace bindings available without making them
active.

Use a project workspace when asking an agent to change a repository. Keep the
agent's role and personal working records in Agent Home. These locations serve
different purposes even when the same agent uses both.

## The locations involved

| Location | Purpose |
|---|---|
| Agent Home | Agent-local guidance, curated memory, notes, and runtime state. It is also the fallback workspace. |
| Project workspace | The project binding and instruction scope for the work. |
| Execution root | The physical root currently used for execution, such as the canonical checkout or a linked worktree. |
| Working directory | The directory where a particular command starts. |

A repository can therefore remain the same workspace while the agent works in
a separate linked worktree. That worktree is a distinct execution location,
and file references must point to the checkout that actually contains the
artifact.

## Attaching, switching, and changing directory

Attaching a workspace makes a binding available to the agent. Switching
activates an existing binding or registered execution root for subsequent
operations. Attaching alone does not switch, and switching does not create a
new checkout.

A shell `cd` changes only that command process. It does not change the active
workspace, loaded workspace guidance, or the base for a later relative patch.
Likewise, setting a command's working directory is not a durable workspace
switch.

Suppose an agent is reviewing repository A and needs to fix repository B.
Running `cd` into B for one command can inspect B, but it leaves the runtime's
workspace context on A. For ongoing work on B, attach it if needed, activate
it, and verify the active execution root before editing.

## Worktrees for parallel changes

A managed Git worktree gives a task its own checkout and branch while retaining
its relationship to the repository. This is useful when one agent reviews a
change while another prepares a fix. Separate worktrees reduce accidental
overwrites in the working copy.

They do not isolate shared Git metadata, external services, or host
credentials. Two agents can still conflict if they operate on the same shared
resource. Agree on ownership and use the reported execution location when
handing off files.

Cleanup is explicit. Leaving a workspace does not remove its worktree;
detaching a binding does not delete project files. Managed worktree removal
checks for conditions such as dirty files and active use. A finished task is
not permission to delete another agent's checkout.

## Organization is not confinement

The active workspace determines defaults for commands and relative file
mutations. It is not a filesystem sandbox. With the host-local backend,
explicit paths and shell commands may reach outside it when host permissions
allow.

An operator's "edit only this directory" instruction remains binding even when
the environment could access more. For enforced isolation, configure the host
or execution environment separately. See
[Security and execution boundaries](./security-and-execution-boundaries.md).

## Next steps

- [Work in a project workspace or worktree](../guides/work-with-workspaces.md)
- [Workspace and execution reference](../reference/workspaces.md)
- [Workspace and execution specification](../spec/workspace-and-execution.md)
- [Results and artifact locations](./results-and-artifacts.md)
