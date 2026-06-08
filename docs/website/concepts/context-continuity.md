---
title: Context continuity
summary: How Holon keeps long-lived agent context coherent without replaying every transcript turn.
order: 12
---

# Context Continuity

Holon agents are meant to continue work across many turns, wakeups, tasks, and
model calls. Context continuity is the product contract that lets an agent keep
the current objective, important results, and provenance visible without
replaying the entire transcript.

Holon does this by treating context as a runtime projection, not as a chat log.
The durable record remains available for audit and recovery, while each model
turn receives a bounded selection of the information that matters now.

## What the Model Sees

A Holon prompt is assembled from several sections with different jobs:

| Section | Purpose |
|---------|---------|
| **Current input and continuation anchor** | The event that woke the agent and the trusted operator intent it continues. |
| **Current work item** | The active objective, durable plan, todo list, and any waiting state. |
| **Working memory** | A compact snapshot of current state and follow-ups derived from runtime records. |
| **Relevant episode memory** | Archived completed work selected by relevance and budget. |
| **Recent runtime evidence** | Recent messages, result briefs, task results, tool executions, and wakeups when they are directly useful. |
| **Execution environment** | The current workspace, runtime capabilities, and scoped guidance. |

These sections are not separate sources of truth competing with each other.
They are projections of runtime evidence. The durable ledger remains the audit
trail; the prompt is the budgeted view used for the next model decision.
When sections overlap, the current work item is the task lifecycle source of
truth. Relevant episode memory is historical evidence and should not override
the active work item's objective, plan, todo list, or waiting state.

## Turns Are the Causal Unit

Holon uses runtime turns as the main way to preserve continuity. A turn is one
activation of an agent: an operator message, a task result, an external wake, a
scheduler tick, or a provider recovery attempt.

The important part is not only the trigger. A turn also links what happened:

- the operator input or external event that caused the turn;
- task results or tool executions observed during the turn;
- result briefs produced for the operator;
- work item updates, waits, and completion reports;
- provenance and trust classification for the inputs involved.

This keeps related facts together. For example, a CI wake can be shown as a
continuation of the original "fix this issue" request instead of replacing
that request as the newest intent. A completion report can stay linked to the
work item it completed instead of becoming an unrelated summary.

## Projection Is Not Transcript Truncation

Simple truncation drops the oldest text once a context window fills. That is
not enough for long-lived agents: old text may contain the operator's original
intent, a completed decision, a task-result fact, or the reason an agent is
waiting.

Holon uses projection instead:

1. **Pin what must remain authoritative**, such as trusted operator intent,
   active work item state, waits, and lifecycle transitions.
2. **Select useful recent turns** by continuation chain, retention priority,
   and budget.
3. **Fold low-value runtime noise**, such as duplicate wakeups, retries,
   fallback attempts, no-op scheduler ticks, or repeated pending polls.
4. **Render older completed work as structured episodes** with source refs and
   authority boundaries.

The result is a shorter prompt that still preserves why the agent is acting,
what it has already done, and which evidence supports the current state.
This turn-based context projection is Holon's intended direction for preserving
continuity without rendering an unbounded transcript.

## Compaction Preserves Provenance

Compaction is a way to keep context bounded; it is not permission to rewrite
history.

When older turn ranges age out of the hot prompt budget, Holon can archive them
as structured episodes. An episode records the covered turn range, source turn
IDs, source references, decisions, results, verification, unresolved items,
operator intents, and model inferences.

By default, prompt assembly recalls only episodes whose covered turn range is
older than the `recent_turns` window. Recent turns provide the high-fidelity
view of current interaction; episodes provide the compressed mid-term archive
for turns no longer shown directly. Diagnostic modes may choose to show both,
but ordinary prompts should avoid replaying the same turn evidence twice.

Model-generated summaries may help describe an episode, but they are evidence,
not authority. They do not change the trust level of an input, overwrite
operator intent, decide work item state, or determine what may be discarded.
The source refs remain available for audit and recovery.

## Work Items, Tasks, Briefs, and Final Answers

Continuity depends on keeping runtime objects distinct:

- **Work items** hold durable objectives, plans, todo lists, waits, and
  completion status.
- **Tasks** represent supervised commands or child agents. Their results can
  wake an agent and continue an older operator request.
- **Briefs** are user-facing or prompt-facing outputs produced by a turn. A
  turn may produce more than one brief, including work item completion reports.
- **Final answers** are what the operator sees for the current interaction.
  They are delivery artifacts, not the only memory mechanism.

Because these objects are linked through turns, the agent can answer questions
such as "what did we do for that issue?" or "why are you waiting?" without
searching an unstructured transcript.

## What Is the User-Facing Contract

Users should be able to rely on these behaviors:

- the current objective and trusted operator intent remain visible across
  wakeups and task-result continuations;
- active work item state is treated as authoritative runtime state;
- compaction keeps provenance and does not silently flatten trust boundaries;
- completed work can be recalled through episode memory and completion
  reports;
- prompt context is bounded, so old transcript text is not always replayed.

The exact internal schemas, token budgets, and retention scores are
implementation details. The visible contract is that Holon preserves continuity
through typed runtime evidence rather than by treating the chat transcript as
the only source of memory.

## See Also

- [Runtime Model](/concepts/runtime-model.md) — Agents, work items, tasks, and wakeups
- [Memory System](/concepts/memory.md) — Working memory, episode memory, durable ledger, and search
- [Trust Boundaries](/concepts/trust-boundaries.md) — Origin and trust classification
- RFC: [Turn-Based Context Projection](https://github.com/holon-run/holon/blob/main/docs/rfcs/turn-based-context-projection.md)
- RFC: [Long-Lived Context Memory](https://github.com/holon-run/holon/blob/main/docs/rfcs/long-lived-context-memory.md)
