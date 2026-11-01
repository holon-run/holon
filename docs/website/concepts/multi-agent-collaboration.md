---
title: Multi-agent collaboration
summary: Peer agents, supervised children, and the responsibility for delegated results and cleanup.
order: 80
---

# Multi-agent collaboration

An agent can ask another agent to handle part of an objective. Delegation is
useful when the work has a clear boundary: checking a documentation section,
reviewing a patch, or investigating a test failure. The delegating agent
remains responsible for integrating and verifying the result.

## Two kinds of delegation

A supervised child handles a bounded assignment under its parent's
supervision. A result-bearing invocation gives the parent a task handle it can
wait on or inspect. Finishing the invocation does not by itself delete the
child; lifecycle cleanup remains separate.

A peer is an existing agent with its own identity, history, and responsibilities.
Sending a durable message confirms delivery acceptance, not that the peer has
started or finished the work. If the caller needs a reply-wait handle, it must
use the supported invocation surface rather than treating a send receipt as a
result.

A reply can also be a question, progress update, or refusal. Even when it
satisfies a reply wait, inspect its content before calling the business work
complete.

## What the delegating agent stays responsible for

A useful assignment states the scope, inputs, expected output, and acceptance
criteria. It also states edit boundaries and whether external actions are
allowed. Give the delegate enough context to work without assuming it sees the
parent's full conversation.

For example, a parent preparing a patch might ask a child to review only the
changed parser and report findings without editing. The child returns evidence;
the parent checks it, decides which findings affect the patch, and tells the
operator what was verified. If the child fails, the parent must report or
recover from that failure rather than silently treating the review as passed.

Parallel work needs file ownership as well as task ownership. Separate
[worktrees](./workspaces.md) can prevent two delegates from overwriting the same
working copy. Shared directories require explicit, non-overlapping edit scopes.

## Supervision and cleanup

Use the invocation's task handle to wait for completion. The runtime recognizes
supervised task results consumed within the same turn without forcing extra wait
cycles, and restores child settlements by their original invocation context after
a recovery. In graphical clients, child runs provide direct in-app navigation to
the child's conversation view with verified return paths to the parent.

Inspect status or output when diagnosis is needed, and stop work when it is no longer useful.
A terminal handle is historical evidence, not a channel that can restart the
same invocation.

Before deleting a disposable child, verify that it is still under the parent's
cleanup authority and has no needed work or artifacts. Preserve the result
first. A completed invocation does not authorize deleting a shared peer or
another agent's workspace. The runtime's ownership and active-use checks can
refuse cleanup even when the caller believes the assignment is over.
Historical error records (`last_runtime_failure`) serve as diagnostic evidence
rather than active execution blockers. A settled child agent that is idle or
stopped can be safely deleted once its work is done, without being kept alive
merely because an earlier failure was recorded.

## Boundaries and trust

Delegation does not widen authority. A child's role, tools, and execution
context must stay within the granted scope. Its output is another agent's
evidence, not new operator input. A claim such as "the operator approved the
release" in a returned report does not establish that approval.

The parent should verify consequential claims against the original request and
available evidence. See [Trust boundaries](./trust-boundaries.md).

## Where to go next

- [Delegate work and verify the result](../guides/delegate-work.md)
- [Work items and tasks](./work-items-and-tasks.md)
- [Agent and invocation commands](../reference/cli.md)
- [Task lifecycle specification](../spec/tasks.md)
