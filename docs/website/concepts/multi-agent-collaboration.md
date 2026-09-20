---
title: Multi-agent collaboration
summary: Roles, boundaries, and trust when one agent delegates work to another.
order: 17
---

# Multi-agent collaboration

An agent can hand part of its work to another agent. The delegating agent stays
responsible for the outcome; the other agent does the work and returns a result.
Think of it as delegating to a teammate: agree on the task, the inputs, and what
"done" means before you start.

## Two kinds of delegation

- **A supervised child** is created for one task and reports back to the agent
  that created it. Use it for a bounded job with a clear finish line.
- **A peer agent** already exists, with its own identity and history. You send it
  a message and it decides what to do. Use it for ongoing responsibilities.

## What the delegating agent stays responsible for

- Framing the task so the delegate has what it needs.
- Deciding when the result is good enough.
- Telling you the outcome, including when the delegate failed.

The delegate reports a result; it does not quietly become the owner of your
objective.

## Boundaries and trust

Delegation does not widen authority. A child agent works with the authority it was
given and cannot escalate on its own. Its output comes back as another agent's
message, not as operator input, and is handled with the same origin and trust
rules as any other source. See [Trust boundaries](./trust-boundaries.md).

## Where to go next

- Steps for delegating: [Delegate work to another agent](/guides/delegate-work.md).
- Agent identity, workspace modes, and supervision controls: [CLI
  reference](/reference/cli.md) and [Workspace and execution
  reference](/reference/workspaces.md).
