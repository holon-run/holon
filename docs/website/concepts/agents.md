---
title: Agents, roles, and guidance
summary: Agent identity, Agent Home, and the instructions that define an agent's responsibilities.
order: 20
---

# Agents, roles, and guidance

An agent is an addressable worker with its own identity, guidance, memory, and
work state. You can return to the same agent from another client without
creating a new identity. A chat window is one way to interact with it; the
runtime owns the agent.

For example, a reviewer agent can inspect a change today and answer a follow-up
tomorrow using its saved work records. Its identity alone does not keep that
review running. Continuing work also needs a runnable objective or a recorded
wait that can resume when the expected event arrives.

## Identity and responsibility

The agent ID identifies the recipient of messages and the owner of runtime
state. A role describes the work that agent should do. Calling an agent
`reviewer` does not automatically give it review instructions, repository
access, or permission to approve a pull request.

Use a persistent agent when a responsibility benefits from accumulated context,
such as maintaining documentation. Use a supervised child for a bounded
delegation, such as checking links in one directory. The parent remains
responsible for accepting the child's result and handling cleanup. See
[Multi-agent collaboration](./multi-agent-collaboration.md).

## What belongs in Agent Home

Agent Home holds the agent's own durable files. Its `AGENTS.md` describes the
role, standing responsibilities, and escalation boundaries. Curated memory
holds stable preferences and facts; notes hold longer reference material.
Runtime-managed records preserve work and execution evidence.

Project code and project conventions belong in the project
[workspace](./workspaces.md). A reviewer that works on two repositories can
keep its review role in Agent Home while each repository supplies its own build
commands and contribution rules. Keeping these scopes separate avoids carrying
one project's conventions into another.

## How guidance fits together

Loaded guidance tells the agent how to work within its authorized scope:

- The agent's `AGENTS.md` defines its long-lived role.
- Workspace guidance defines local conventions for the current project.
- The current operator request defines the task and its acceptance target,
  subject to higher-priority runtime constraints.
- A [skill](./skills-and-tools.md) supplies a workflow when the task calls for it.

These sources have scopes and precedence. A project rule to run tests does not
authorize a release when the operator requested a read-only review. An ordinary
file opened during research does not acquire the authority of loaded guidance.
See [Trust boundaries](./trust-boundaries.md).

An [Agent Template](./agent-templates.md) initializes a new agent's role and
optional skills. Once created, the agent has its own files and history. To
change an existing agent's responsibilities, update its guidance deliberately
rather than trying to reapply a template over its state.

## Lifecycle and limits

An agent can be idle or waiting while its identity and work records remain
available. Closing a client is different from stopping the agent or daemon.
Deleting an agent is a separate lifecycle action, not a way to close a chat.
Inspect outstanding work and ownership before cleanup.

When created without an explicit initial message (through the Web GUI, API, or
`holon agent create`), the agent runs a default bootstrap initialization turn
so its conversation view is ready immediately. One-shot command runs bypass this
turn to accept the caller's prompt directly.

Persistent identity also does not mean unlimited recall. Each model call has
bounded context, and an interrupted command may need recovery even when the
objective survives. [Context continuity](./context-continuity.md) explains
these limits.

## Next steps

- [Create an agent from a template](../guides/create-agent.md)
- [Customize an existing agent](../guides/customize-agent.md)
- [Agent and lifecycle commands](../reference/cli.md)
- [Agent state specification](../spec/agent-state.md)
