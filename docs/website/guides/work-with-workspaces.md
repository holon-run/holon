---
title: Work in a project workspace or Git worktree
summary: Attach your local repository in conversation, isolate coding tasks in managed Git worktrees, verify changes safely, and clean up without touching your main working directory.
order: 60
---

# Work in a project workspace or Git worktree

When you ask an AI coding agent to inspect or modify your code, letting it work directly in your primary working directory is risky. An uncommitted scratch file, a local debugging configuration, or an unintended branch switch can disrupt your flow or pollute your repo.

Holon solves this by treating workspaces and Git worktrees as first-class runtime resources. Instead of cloning a huge repository or risking dirty working trees, you can ask your agent in conversation to attach your project and spin up a linked Git worktree in seconds. The agent works on an isolated branch with its own HEAD and working tree, while sharing the same underlying Git object store.

This guide shows how to attach a project, activate an isolated worktree for a task, run local verification, and safely clean up—all through natural conversation in the Web GUI without memorizing CLI syntax or sending raw JSON tool payloads. For the architectural concepts, see [Workspaces and working directories](../concepts/workspaces.md).

## Before you start

- A running Holon instance with the [Web GUI](./use-web-gui.md) open (default `http://localhost:8080`) and a [configured model](./configure-models.md).
- An existing agent (such as `developer` or `main`).
- A local Git repository on your machine that you have filesystem permissions to edit (for example `/home/user/src/github.com/my-org/my-app`).
- No need to run terminal commands or craft raw tool calls: your agent can handle workspace attachment, worktree creation, and safe cleanup directly in conversation.

## Steps

### 1. Attach and activate your project workspace

In the Web GUI chat, ask your agent to attach your local project repository and make it the active workspace:

> "Please attach my local repository at `/home/user/src/github.com/my-org/my-app` as a workspace and switch to it for upcoming tasks."

**What happens behind the scenes:**
- The agent calls its built-in `AttachWorkspace` tool to bind the project directory to its profile, followed by `SwitchWorkspace` to set the project as its active execution root.
- From this moment on, relative file edits (such as code patches) and terminal commands default to this repository root instead of the agent's personal home directory.
- Note that running a shell `cd` in a command only affects that single child process. Durable workspace switching must be performed through the runtime's workspace activation.

### 2. Request an isolated Git worktree for your task

Before allowing the agent to edit files or run refactors, instruct it to create an isolated Git worktree based on a clean ref:

> "Create a new Git worktree from `main` on branch `feat/user-auth` for this task. Do all edits and verification inside this worktree so my main working directory stays clean."

**What happens behind the scenes:**
- The agent invokes `CreateWorktree`. Holon creates a linked Git worktree in a managed directory and switches the agent's active execution root to that worktree.
- The worktree shares the local `.git` repository, so creation is instantaneous and consumes virtually zero extra disk space for Git history.
- The agent's branch `feat/user-auth` has an independent working tree and HEAD. Your primary checkout remains completely unaffected even if you have uncommitted changes or are actively coding on another branch.

### 3. Implement and verify changes inside the worktree

Now instruct the agent to carry out the coding task and run project tests:

> "Please implement the new authentication handler in `src/auth.rs`, add unit tests, and run `cargo test --lib auth` to make sure everything passes."

**What happens behind the scenes:**
- The agent makes edits within the isolated worktree directory.
- Test suites, linters, and compilers execute strictly inside the worktree's execution root.
- The agent commits the changes to the dedicated `feat/user-auth` branch once tests succeed, or leaves clean staged commits ready for your pull request.

### 4. Safely clean up the worktree after completion

Once the changes are committed, pushed, or turned into a pull request, ask the agent to return to the project root and clean up the temporary worktree:

> "The PR has been submitted. Please switch back to the main repository workspace and safely remove the `feat/user-auth` worktree, keeping the branch intact."

**What happens behind the scenes:**
- The agent switches its active workspace back to the canonical project checkout.
- It then calls `RemoveWorktree` with `branch_policy: "keep"` to remove the temporary worktree directory from disk while preserving the Git branch and commit history.
- **Runtime safety guardrails:**
  - **Dirty tree protection:** If there are uncommitted modifications in the worktree, the runtime automatically blocks removal to prevent data loss.
  - **Active occupancy protection:** If background tasks or running processes are still executing in that worktree, cleanup is refused until they finish.
  - **Path validation:** Only registered, Holon-managed worktrees can be cleaned up; arbitrary filesystem paths are rejected.

## Key mechanisms to understand

### Execution root vs. shell `cd`

A common source of confusion in agent tooling is the difference between changing directory in a subshell and switching the runtime execution root:

- **Shell `cd`:** Changes the current working directory for that single process execution. The next command or file patch tool reverts to the agent's active root.
- **Workspace activation (`SwitchWorkspace`):** Durably switches the runtime's execution root, the base directory for relative patch tools (`ApplyPatch`), and the resolution scope for project-level `AGENTS.md` instructions.

### Why Git worktrees beat fresh clones

| Approach | Speed | Disk Usage | Unpushed Context |
|---|---|---|---|
| Direct main checkout | Instant | Zero | High risk of polluting uncommitted work |
| Full `git clone` | Slow (minutes on large repos) | High (duplicates full history) | Misses local unpushed branches |
| **Git worktree** | **Instant (milliseconds)** | **Minimal (shares `.git` storage)** | **Full access to local refs and commits** |

## Optional: CLI alternatives for automation

While natural language interaction in the Web GUI is recommended for daily development, Holon also provides CLI commands for scripted workflows or headless setups:

```bash
# 1. Attach a repository to an agent
holon workspace attach --agent developer /home/user/src/github.com/my-org/my-app

# 2. Inspect an agent's active workspace and execution root
holon agent get developer --detail full

# 3. Detach a workspace when no longer needed
holon workspace detach --agent developer ws_12345

# 4. Exit to Agent Home
holon workspace exit --agent developer
```

## Next steps

- Read [Workspaces and working directories](../concepts/workspaces.md) for deeper conceptual details.
- See [Build a Docs Steward agent](./build-docs-steward.md) to see how an agent audits documentation drift inside a dedicated worktree.
- See [Build a Code Reviewer agent](./build-code-reviewer.md) to learn how agents review PRs using isolated checkouts.
- Consult the [Workspace reference](../reference/workspaces.md) for full parameter schemas and error codes.
