---
title: Context continuity
summary: How durable work state helps an agent resume without replaying all history, and what interruptions still require recovery.
order: 100
---

# Context continuity

Context continuity lets an agent resume an objective across activations using
saved work state and relevant evidence. Each model call receives a bounded
view. The runtime does not replay every transcript or load every file the
agent has ever read.

For example, after waiting for CI, an agent needs the original acceptance
criteria, the revision under test, and the new CI result. It usually does not
need the full output of every earlier shell command.

## What the model sees

An activation can include the current input and its provenance, the objective
it continues, current work state, relevant recent results, selected historical
evidence, and the execution environment with its guidance.

The active WorkItem is the record for the current objective, plan, and progress.
Older summaries help explain how the work arrived there. They do not override
updated scope or a later operator decision. When exact details matter, the
agent should reopen the referenced evidence.

<a id="turns-are-the-causal-unit"></a>

## Connecting a result to the right request

A task result may arrive after the agent has handled another message. Holon
keeps the result connected to its invocation and work ownership so that it can
continue the relevant request. A new event does not automatically become a new
operator objective.

This is why a CI notification should lead back to the work that requested the
CI run. The agent must still check that the run and revision are the intended
ones before accepting its result.

<a id="projection-is-not-transcript-truncation"></a>
<a id="compaction-preserves-provenance"></a>

## Bounded context and recall

The runtime selects current work and useful evidence instead of simply using
an ever-growing chat log. Older work can be represented by summaries and
references. Some detail will therefore be absent from the current prompt.

Summaries preserve a route back to evidence; they are not independent approval
or a replacement for the work record. An external instruction remains external
when summarized. [Memory](./memory.md) explains what can be searched and which
files are loaded automatically.

<a id="work-items-tasks-briefs-and-final-answers"></a>

## Records that make recovery useful

A WorkItem should preserve scope, decisions, acceptance criteria, and the next
action. Task handles identify executions and their results. A completion report
states the outcome and verification. Artifact references identify the actual
files, including the worktree that contains them.

These records let the agent recover without assuming it remembers an old
conversation. A vague plan such as "continue fixing things" does not provide
enough information. Nor does a final reply replace updating an unfinished
objective's state.

## How Holon keeps work continuous

Distinguish three situations:

| Interruption | What to expect |
|---|---|
| Client disconnects | The daemon owns runtime work and can continue while it remains running. Reconnect to inspect state and results. |
| Daemon stops or restarts | Persisted objectives and evidence support recovery. Inspect current state before assuming execution resumed. |
| Command or provider request is interrupted | Check its recorded outcome and any side effects before deciding whether to retry. Durable intent does not guarantee an uninterrupted process. |

A reconnect should begin with the current WorkItem and task state, not with an
assumption that every previously running process survived. Retrying an action
that already changed an external system can duplicate that change.

## What is the user-facing contract

You can rely on explicit work records and provenance to understand what the
agent is continuing. You should not rely on unlimited recall, replay of every
transcript, or automatic success after an interruption.

For work that must resume later, preserve the objective and acceptance criteria,
record waits through the runtime, and keep required artifacts in durable
locations. If evidence is missing or a process failed, the agent should say so
and identify the recovery step.

## See also

- [Track long-lived work](../guides/run-long-lived-task.md)
- [Observe progress and results](../guides/observe-agent-work.md)
- [Memory](./memory.md)
- [Wake and continuation specification](../spec/wake-and-continuation.md)
- [Work item specification](../spec/work-items.md)
