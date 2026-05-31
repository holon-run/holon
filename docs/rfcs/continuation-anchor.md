---
title: RFC: Continuation Anchor
date: 2026-05-31
status: draft
---

# Continuation Anchor

This RFC defines a small context contract for preserving operator intent across
context trimming, provider fallback, and recovery turns.

It extends, but does not replace:

- [Long-Lived Context Memory](./long-lived-context-memory.md)
- [Turn-Local Context Compaction](./turn-local-context-compaction.md)
- [Turn Model Lineage And Recovery](./turn-model-lineage-and-recovery.md)
- [Work Item Runtime Model](./work-item-runtime-model.md)

## 1. Problem Shape

Holon can start a new runtime turn from many sources:

- trusted operator input
- task results
- scheduler wakes
- external trigger wakes
- provider fallback or recovery messages

After context trimming or fallback, the newest visible input may be a runtime
continuation message rather than the operator request that defined the task.
If the prompt does not keep a stable operator-intent anchor, the model can
mistake the runtime message for the task source and lose important details such
as the requested deliverable, scope boundary, or apparent response language.

This is not only a language-selection issue. It is a task-continuity issue: the
runtime must preserve enough trusted context for the agent to know what it is
continuing.

## 2. Goals

- Preserve the latest trusted operator input as an intent anchor when it is
  needed for continuation.
- Make the relation between `current_input` and the latest trusted operator
  input explicit.
- Use the current WorkItem as the authoritative continuation record when one is
  active.
- Keep recovery and fallback messages from replacing trusted operator intent.
- Avoid creating a second continuation-state system beside WorkItems.
- Require no new agent-maintained continuity tool.

## 3. Non-Goals

- Do not define a full task summary or todo schema.
- Do not duplicate WorkItem objective, plan, todo, or wait state.
- Do not make runtime messages authoritative for operator intent.
- Do not require an LLM summarization pass before every turn.
- Do not solve all transcript retention or long-term memory policy in this RFC.

## 4. Terms

### Trusted Operator Input

A message admitted as operator-originated and trusted according to Holon's
provenance and admission policy.

### Current Input

The message or event that started the current runtime turn. It may be trusted
operator input, or it may be an internal runtime continuation such as recovery,
fallback, task result, wake, or scheduler input.

### Continuation Anchor

A small prompt frame that tells the model which trusted operator input or
WorkItem anchors the task being continued, and how that anchor relates to the
current input.

## 5. Proposed Contract

### 5.1 Always Classify The Current Input Relation

Prompt context should expose whether `current_input` is:

- the latest trusted operator input
- a trusted operator override newer than previous state
- a runtime continuation that must not replace operator intent

This relation is more important than a generated task summary. It tells the
model whether to treat the current input as the task source or as a wake/retry
event for an existing task.

### 5.2 No Active WorkItem: Pin Trusted Operator Intent When Needed

When no WorkItem is active, the runtime should keep the latest trusted operator
input available as the lightweight continuation anchor whenever the current
turn is not itself that operator input.

For short operator messages, the anchor should preserve the message body
verbatim within budget. For long messages, it should preserve a bounded excerpt
large enough to retain the requested deliverable and scope.

Example shape:

```text
## continuation_anchor
Latest trusted operator input:
<operator message or bounded excerpt>

Current input relation:
current_input is a runtime recovery/fallback continuation, not a new operator
request. Continue the latest trusted operator input above.
```

If the current input already is the latest trusted operator input, the prompt
may omit the duplicate body and only record that the anchor is the current
input.

### 5.3 Active WorkItem: Use Existing WorkItem Projection

When a current WorkItem is active, the WorkItem is the authoritative durable
record for what is being continued. The prompt already has a WorkItem
projection for that state; the continuation anchor should not render another
`Current WorkItem` line or repeat WorkItem fields.

In this case, the anchor only needs to classify how `current_input` relates to
the existing WorkItem-backed continuation:

Example shape:

```text
## continuation_anchor
Latest trusted operator input: msg_abc.
Current input relation: runtime wake, not operator override.
```

If a new trusted operator input conflicts with the active WorkItem, normal
operator authority rules apply: the trusted operator input can refine,
override, or redirect the task. The runtime should make that relation visible
rather than silently merging the two.

### 5.4 Recovery And Fallback Must Not Become Task Sources

Recovery and fallback turns should be explicit that they are runtime
continuations. They may explain why the previous turn stopped, but they must
not become the task objective.

The prompt builder should therefore render recovery/fallback context together
with either:

- the existing active WorkItem projection, or
- the latest trusted operator input anchor.

### 5.5 Budget Priority

When prompt budget is tight, keep these before low-authority recent transcript
details:

1. current trusted operator input, when present
2. existing active WorkItem projection, when present
3. latest trusted operator input anchor, when current input is runtime-originated
4. recovery/fallback relation note
5. recent result or brief needed to continue execution

The anchor does not need to preserve arbitrary old conversation. It only needs
to prevent the task source from being trimmed away or hidden behind a runtime
continuation.

## 6. Implementation Boundary

The first implementation should live in the context builder and use existing
runtime data:

- message log entries and trust/origin classification
- current input metadata
- current WorkItem projection
- existing latest result and recent brief surfaces

It should not add a new agent-facing update tool. Agents should not have to
manually maintain continuation state for ordinary short tasks.

## 7. Acceptance Scenarios

The implementation should cover these scenarios:

1. A fallback or recovery turn after a trusted operator request with no active
   WorkItem still shows the operator request as the task anchor.
2. A fallback or recovery turn with an active WorkItem points to the WorkItem as
   authoritative and does not duplicate WorkItem fields.
3. A trusted operator follow-up is classified as the current task source, not
   as a runtime continuation.
4. A runtime wake or task-result turn is classified as continuation input and
   does not override trusted operator intent.
5. Under a tight context budget, the continuation anchor is retained ahead of
   lower-authority transcript history.

