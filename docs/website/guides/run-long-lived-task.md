---
title: Run unattended long-lived tasks and recover across sessions
summary: Assign long-running coding goals in conversation, let the agent execute in background WorkItems with zero-token sleep, and resume seamlessly after disconnecting or switching devices.
order: 70
---

# Run unattended long-lived tasks and recover across sessions

When you ask an AI coding agent to undertake a substantial project—such as refactoring an authentication system, migrating database schemas, or executing an extensive end-to-end test suite—the work can take hours. You shouldn't have to keep a terminal window open or stay glued to your screen while the agent runs.

Traditional CLI tools often fail here: closing your laptop lid, disconnecting from Wi-Fi, or accidentally terminating a shell aborts the running process and discards the model's working memory.

Holon solves this with first-class, long-lived **WorkItems**. You assign goals in natural language in the Web GUI, and the agent breaks down the objective into a persistent checklist, runs compilation and test commands in managed background tasks, and enters dormant, zero-token sleep while waiting for tasks or approvals. You can close your laptop, switch to your mobile device, or reopen the Web GUI hours later to find the agent's progress preserved and ready for review.

This guide walks through starting a durable task, letting it run unattended in the background, recovering state after disconnecting, and accepting the final deliverables. For the underlying runtime architecture, see [Work items reference](../reference/work-items.md) and [Context continuity](../concepts/context-continuity.md).

## Before you start

- A running Holon instance with the [Web GUI](./use-web-gui.md) open (default `http://localhost:8080`) and a [configured model](./configure-models.md).
- An existing agent (such as `developer` or `main`).
- An attached project repository where you want the agent to work (see [Work in a project workspace or Git worktree](./work-with-workspaces.md)).
- No need to manage background processes (`nohup`, `tmux`) or craft raw JSON tool calls: your agent creates and tracks its own WorkItems through conversational instructions.

## Steps

### 1. Define the objective and request a durable WorkItem

In the Web GUI chat, give your agent an overarching goal along with clear scope boundaries and approval checkpoints:

> "I need to refactor our user authentication module to support OAuth2 PKCE. Please track this in a dedicated WorkItem: inspect the existing codebase, draft an implementation plan with acceptance criteria, and ask for my review before modifying any code. Do not push commits or open a PR yet."

**What happens behind the scenes:**
- The agent calls `CreateWorkItem` to establish a durable record in its local SQLite WAL database.
- It seeds a plan artifact (`plan.md`) describing the scope, acceptance criteria, and architectural decisions.
- It populates an initial progress checklist (`todo_list`) with structured milestones (e.g., codebase inspection, plan approval, implementation, test verification).
- The WorkItem's status is set to `needs_input`, signaling that the agent is waiting for your review.

### 2. Review the plan and approve implementation

When the agent finishes its preliminary audit, it reports its findings and asks for approval. Review the proposed scope in the Web GUI:

> "The plan looks solid. Please proceed with the implementation, add integration tests for PKCE token exchange, and run the test suite."

**What happens behind the scenes:**
- The agent updates the WorkItem status to `ready` and marks the first milestone complete.
- It proceeds to edit files and write tests within its active workspace or isolated Git worktree.
- If a subsequent decision arises (e.g. choosing between two token storage formats), the agent does not guess: it marks `plan_status="needs_input"`, registers a wait (`WaitFor(wake="operator_input")`), and yields the turn.

### 3. Let background commands run while you disconnect

Long-running commands—such as compiling large Rust crates, downloading npm dependencies, or executing browser test suites—should not block your interactive session.

When the agent starts a long-running command:

> "Running `cargo test --test oauth_integration`. This may take 15 minutes. You can safely close your browser or disconnect."

**What happens behind the scenes:**
- **Command promotion (`command_task`):** Any non-interactive command that runs beyond a few seconds is automatically promoted by Holon into a managed background command task.
- **Zero-token sleep (`WaitFor`):** The agent records the task ID and calls `WaitFor(wake="task_result", resource=TASK_ID, delivery="silent")`.
- **Resource conservation:** While the background process executes, the agent process yields its turn. It consumes zero LLM prompt tokens and no model inference bandwidth while waiting for the test runner to finish.

### 4. Close your browser and reconnect from another session or device

You can safely close the Web GUI tab, shut your laptop lid, or leave your desk:

1. **The daemon keeps working:** The Holon daemon manages the background process independently of your browser session.
2. **Reopening the Web GUI:** When you return, open `http://localhost:8080` (or open the Holon mobile client).
3. **Inspect the live state:** Select your agent and view the active WorkItem. The UI displays real-time checklist progress, terminal output snippets, and any completed milestones.

### 5. Review final verification and accept the completion brief

Once all checklist items are completed and tests pass, the agent completes the work:

> "All 18 PKCE integration tests passed. I have generated a completion report with full test logs and a diff summary."

**What happens behind the scenes:**
- The agent runs final lint and test checks to verify repository hygiene.
- It calls `CompleteWorkItem`, marking the WorkItem state as `completed`.
- The agent delivers a non-empty operator-facing completion report that is promoted as the canonical WorkItem result brief, linking changed files, test output, and next steps.

## Key mechanisms to understand

### WorkItems vs. transient chat sessions

| Feature | Holon WorkItems | Ordinary AI Chat / Transient CLI |
|---|---|---|
| **Persistence** | Stored in local SQLite WAL ledger; survives browser reloads and reboots | Kept only in browser memory or active terminal stdout |
| **Progress tracking** | First-class checklist (`todo_list`) updated incrementally as steps finish | Unstructured conversational text; easily forgotten or truncated |
| **Wait handling** | Event-driven sleep via `WaitFor`; zero token cost while waiting | Busy polling or prompt timeouts that burn tokens |
| **Multi-session recovery** | Inspect and resume the exact same task from desktop Web GUI, CLI, or mobile | Disconnecting terminates the run or forces starting over |

### How Holon handles interruptions

| Interruption | Recovery Behavior |
|---|---|
| **Browser tab closed / Wi-Fi lost** | Holon daemon continues background execution; reconnect anytime to view progress. |
| **Agent waiting for human decision** | WorkItem remains in `needs_input` state without consuming CPU or tokens until you reply. |
| **Daemon process restarted** | On startup, Holon re-reads persisted WorkItems and task records; active tasks can be inspected or resumed without state drift. |
| **Host machine reboot** | Daemon restarts with persisted WorkItem states; rerun any uncompleted background commands from the recorded checklist milestone. |

## Optional: CLI alternatives for headless setups

If you are working on a remote server or headless box without the Web GUI, you can inspect and manage WorkItems using the CLI:

```bash
# List all active and open WorkItems for an agent
holon work-item list --agent developer

# Inspect a specific WorkItem's plan, checklist, and status
holon work-item get WORK_ITEM_ID --agent developer

# Check running background command tasks
holon task list

# Tail the agent's recent activity log
holon tail --agent developer --limit 15
```

## Next steps

- [Schedule recurring agent checks and timers](./schedule-agent-work.md) — set up automated recurring wakeups and cron schedules.
- [Work in a project workspace or Git worktree](./work-with-workspaces.md) — isolate long tasks inside dedicated Git branches.
- [Observe agent work](./observe-agent-work.md) — monitor agent tasks, tool calls, and output streams in real time.
- [Work items reference](../reference/work-items.md) — deep dive into WorkItem lifecycle states, transitions, and API contracts.
