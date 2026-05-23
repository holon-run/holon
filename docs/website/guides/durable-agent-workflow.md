---
title: Durable agent workflow
summary: "The end-to-end durable agent story: create an agent, start long-running work, survive disconnects, wait for events, and deliver final briefs."
order: 5
---

# Durable agent workflow

Holon's defining difference from one-shot CLI tools is **durability**: agents
keep working across terminal sessions, survive disconnects, wait for external
events, and resume when conditions are met. This guide walks through the full
lifecycle.

## What makes it durable?

A Holon agent session persists these things independently of any client
connection:

- **Agent identity and home** — each agent owns `~/.holon/agents/<name>/`
  with its own `AGENTS.md`, skills, and long-lived memory
- **Work queue** — WorkItems, queued messages, and blocked/waiting state
- **Task lifecycle** — running commands and child-agent delegations continue
  after the client disconnects
- **Sleep/wake state** — agents sleep when idle and wake on events or operator
  input

The daemon keeps the runtime alive. Clients (TUI, HTTP, CLI one-shots) connect,
submit work, inspect progress, and disconnect — the work keeps running.

## The workflow end-to-end

### 1. Start the daemon

All durable work requires the Holon daemon:

```bash
holon daemon start
holon daemon status
```

The daemon creates a default agent at `~/.holon/agents/main/` and listens on a
local Unix socket.

### 2. Create a specialized agent

For focused durable work, create a named agent with a template:

```bash
holon agent create builder --template holon-developer
```

This creates `~/.holon/agents/builder/` initialized with the developer
template, which includes code-editing skills.

### 3. Start a WorkItem-backed piece of work

Connect to the agent through the TUI and start tracking work:

```bash
holon tui
```

In the TUI, tell the agent what to work on. The agent can create a **WorkItem**
— a durable objective tracked with a plan, todo list, and completion criteria.
WorkItems survive TUI disconnects and daemon restarts.

Example prompt:

```
I need to refactor the error handling in src/runtime/turn.rs.
Create a work item, plan the approach, and start implementing.
```

The agent creates the WorkItem, writes a plan, and begins working. You can
disconnect the TUI (`Ctrl+C`) at any point — the agent continues.

### 4. Check progress from another session

Reconnect later or from another terminal:

```bash
# Quick status check (no TUI needed)
holon status

# See what the agent is doing
holon agent status builder

# Reconnect the TUI to interact
holon tui
```

### 5. Wait for external events

Long-running work often needs to wait. Holon handles three kinds of waiting:

#### Waiting for commands to finish

When the agent runs a shell command (build, test, lint), the command becomes a
background task. The agent can continue other work or sleep until the task
completes:

```
> Run cargo test and wait for the results

[Agent runs tests as a background task, sleeps, and wakes when tests finish]
```

#### Waiting for operator input

When the agent needs a decision, it sets the WorkItem to
`plan_status=needs_input` and sleeps:

```
> I've found two possible approaches for the error refactor.
  Option A: use thiserror
  Option B: manual Display impls
  Which should I use?
```

The agent sleeps, leaving the WorkItem in "needs input" state. You can respond
later through the TUI, CLI prompt, or HTTP API.

#### Waiting for external triggers

For CI, webhooks, or scheduled events, Holon uses external triggers. The agent
sets `blocked_by` on the WorkItem with a description of what it's waiting for:

```
> I've pushed the branch. Let's wait for CI to complete before merging.
  [blocked_by: github CI check on feature/error-refactor]
```

When CI completes and the configured trigger fires, the agent wakes, checks the
CI status, and continues — or updates the WorkItem if CI failed.

### 6. Handle interruptions

Durable work survives common interruptions:

| Scenario | What happens |
|----------|-------------|
| TUI disconnects | Agent keeps running; daemon stays up |
| Daemon restarts | Agent state, WorkItems, and agent homes persist on disk |
| Machine reboots | Start `holon daemon start` and work resumes |
| Command task still running | Task continues in background; agent can inspect or wait |

### 7. Receive the final brief

When the WorkItem is complete, the agent writes a **completion brief** — a
structured summary of what was done, why, and the verification result:

```
WorkItem complete: refactor error handling in src/runtime/turn.rs

Changes:
- Replaced manual error strings with thiserror derive macros
- Added structured error variants for turn, queue, and task errors
- Updated 12 call sites to use new error types

Verification: cargo test --all-targets passes (124 tests, 0 failures)
```

Check completed work at any time:

```bash
holon transcript          # Full conversation history
holon agent status builder  # Agent state and recent activity
```

## When to use durable workflows

| Use case | Approach |
|----------|---------|
| Multi-step code changes | WorkItem with plan and todo list |
| CI-driven workflows | blocked_by + external trigger |
| Review cycles | needs_input for operator review |
| Multi-session projects | Named agent with workspace |
| Background automation | Daemon + agent + trigger |

For quick one-shot tasks that don't need durability, use `holon run` instead.

## See also

- [Work items guide](/guides/work-items/) — WorkItem lifecycle and best practices
- [Multi-agent collaboration](/guides/multi-agent/) — delegate work to child agents
- [CLI reference](/reference/cli/) — full command surface
- [Runtime model](/concepts/runtime-model/) — the concepts behind durability
- [Integration guide](/guides/integration/) — HTTP API for automation
