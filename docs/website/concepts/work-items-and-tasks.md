---
title: Work items and tasks
summary: Durable objectives and supervised execution handles, including planning, waits, ownership, and completion.
order: 60
---

# Work items and tasks

A WorkItem records a durable objective. A Task tracks a particular execution,
such as a background test command or a delegated agent invocation. One
WorkItem can involve several tasks, and a short task can run without a
WorkItem.

For example, "fix the failing build and verify the patch" is an objective.
The build command is a task. A successful build supplies evidence toward the
objective; it does not decide whether the patch meets every requirement.

## When to track an objective

Use a WorkItem when work must survive across turns, wait for CI or a human
decision, or keep progress and acceptance criteria available for later review.
It is also useful when the operator explicitly asks to track a list of
decisions over several conversations.

A short explanation, a bounded inspection, or a change that can be completed
and verified in the current turn usually does not need one. The number of
steps alone is not the test.

Tracking a proposal does not authorize implementation. A WorkItem can preserve
options and questions while waiting for the operator to choose.

## What the work record preserves

The objective states the intended outcome. The plan records scope, acceptance
criteria, and the chosen approach. The checklist records meaningful progress.
Evidence references point to results that the agent can reopen.

These records should answer "what remains?" after a context reset. They should
not become a transcript of every tool call. When the approach changes, update
the plan; when a test finishes, record the result and what it means for
acceptance.

## Focus, readiness, and waiting

"Current" identifies the agent's focus. It does not, by itself, mean the item
is running or ready to run. An open item can be waiting for operator input,
blocked on an outside event, or temporarily yielded while another item runs.

A recorded wait specifies the condition under which a continuation should
resume. Writing "waiting for CI" in a plan or ending a reply does not register
that wait. Readiness and waiting are runtime state, managed through the
work-item and wait operations.

Switching focus also does not move an existing task to another WorkItem.
The task retains the owner captured at creation. This matters when a slow
test finishes after the agent has started considering a different objective:
the result still belongs to the original work.

See [Events, waits, and wakeups](./events-and-waits.md) for how task results and
external notifications resume work.

## What a task tells you

A task handle lets the agent inspect status and output and, where supported,
send input or stop the execution. Some handles represent a command; others
represent delegated work or a wait for an existing agent's reply. Interpret
completion according to the kind of handle.

For example, a peer reply can satisfy a reply-wait handle while saying that
the requested review is still pending. A command can exit successfully while
producing no expected artifact. Read the result before accepting the outcome.

## Completing the objective

Complete a WorkItem when the requested outcome has been checked and the
verification status is known. The completion report should state the outcome,
deliverables, and any limits that affect their use.

Ending one execution is not enough. If implementation is still authorized and
unfinished, keep the objective open with a recoverable next step. If work
cannot proceed, record the specific wait instead of claiming that it will
continue automatically.

## Next steps

- [Track and complete long-lived work](../guides/run-long-lived-task.md)
- [Inspect progress and results](../guides/observe-agent-work.md)
- [Work item reference](../reference/work-items.md)
- [Task specification](../spec/tasks.md)
