---
title: RFC: Turn-Based Context Projection
date: 2026-05-31
status: draft
---

# Turn-Based Context Projection

This RFC proposes making runtime turns the primary unit for cross-turn prompt
projection and long-lived context compaction.

It extends, but does not replace:

- [Continuation Anchor](./continuation-anchor.md)
- [Long-Lived Context Memory](./long-lived-context-memory.md)
- [Turn-Local Context Compaction](./turn-local-context-compaction.md)
- [Turn Model Lineage And Recovery](./turn-model-lineage-and-recovery.md)
- [Work Item Runtime Model](./work-item-runtime-model.md)

## 1. Problem Shape

Holon currently projects cross-turn context through several parallel surfaces,
including recent messages, recent briefs, latest result, recent tool
executions, relevant episodes, and WorkItem state.

Those surfaces are useful, but they do not share one first-class causal unit.
This can make prompt reconstruction ambiguous:

- an operator input may still be visible while the result brief that answered
  it has been trimmed
- a result brief may be visible while the operator input or task result that
  caused it has been trimmed
- provider fallback, retry, timer, or no-op wake turns can consume recent
  context budget even though they carry little task meaning
- task-result and external-event turns can become visible as the newest input
  even though they only continue an older operator intent
- model-generated summaries can flatten authority by merging operator input,
  external data, task output, and assistant inference into one prose record

The runtime already has a better natural unit:

```text
activation -> turn -> side effects / result / wait / completion
```

The prompt should therefore recover recent work from turn records rather than
from unrelated message and brief windows.

## 2. Goals

- Treat the runtime turn as the primary cross-turn projection unit.
- Preserve the causal relation between operator input, runtime/task/external
  input, assistant result briefs, tool references, WorkItem changes, and wait
  or completion transitions.
- Keep trusted operator intent and WorkItem state from being displaced by
  provider fallback, retry, timer, duplicate wake, or bookkeeping turns.
- Make prompt retention priority rule-based and provenance-aware before using
  any LLM-generated summaries.
- Allow LLM-generated compaction as a semantic recap while keeping runtime
  provenance and authority boundaries intact.
- Provide a clearer mental model than independent `recent_messages` and
  `recent_briefs` windows.

## 3. Non-Goals

- Do not replace append-only message, brief, task, or tool ledgers.
- Do not require an LLM classification pass for every turn.
- Do not make LLM summaries authoritative for trust, provenance, WorkItem
  state, or operator intent.
- Do not define the final persisted storage schema for every turn-linked
  object.
- Do not solve in-turn provider conversation growth; that remains covered by
  [Turn-Local Context Compaction](./turn-local-context-compaction.md).

## 4. Terms

### Physical Turn

One runtime activation and execution attempt. A physical turn may be caused by
operator input, task result, external event, timer, scheduler tick, provider
fallback, retry, or recovery.

### Semantic Turn Projection

A prompt-facing projection of one or more related physical turns that preserves
the meaningful causal chain for the current continuation.

For example, several provider fallback and retry physical turns may be folded
under the operator-intent turn they were trying to continue.

### Continuation Chain

The set of turns, task ids, external event ids, WorkItem transitions, and
result briefs that continue the same operator intent or WorkItem objective.

### Turn Retention Priority

A runtime-computed priority used by prompt projection to decide whether a turn
is pinned, rendered, summarized, folded, or omitted from the model-visible
context.

## 5. Proposed Contract

### 5.1 Record Turns As Causal Containers

The durable runtime ledger should retain the existing detailed object logs, but
prompt assembly should be able to view them through turn records.

Suggested turn-record shape:

```text
turn_record:
  turn_id
  activation:
    trigger_kind
    trigger_refs
  provenance:
    origin
    trust
  operator_input_refs
  task_result_refs
  external_event_refs
  tool_execution_refs
  produced_brief_refs
  latest_result_ref
  work_item_delta
  wait_delta
  completion_delta
  provider_attempt_refs
  continuation_chain_id
  retention:
    priority
    reasons
```

The turn record does not need to duplicate full object bodies. It should keep
stable references and enough bounded summary fields for prompt assembly.

### 5.2 Link Operator Inputs And Briefs Through Turns

Result briefs should be associated with the turn that produced them, and that
turn should reference the inputs it was responding to.

The prompt should be able to render:

```text
Turn T:
- trigger: trusted operator input
- operator asked: ...
- result: ...
- work item delta: ...
```

or:

```text
Turn U:
- trigger: task_result
- continues operator turn: T
- task result: cargo test passed
- result: verification complete
```

This is preferable to separately rendering a recent operator message list and a
recent brief list with no guaranteed linkage.

### 5.3 Classify Current Input Relation Before Rendering History

Every prompt should make the current turn relation explicit:

- current input is a new trusted operator intent
- current input is a trusted operator override or refinement
- current input is a task result continuing an existing chain
- current input is an external event continuing or waking an existing chain
- current input is a timer or scheduler wake
- current input is provider fallback, retry, or recovery and must not replace
  operator intent

This relation is a structural runtime fact. It should not require an LLM to
infer it from prose.

### 5.4 Compute Baseline Retention From Runtime Facts

The runtime should compute a conservative retention priority from provenance,
trigger kind, and state delta.

Suggested priorities:

```text
pinned:
  - current turn
  - continuation anchor turn
  - current WorkItem source or transition turn
  - latest trusted operator intent turn

high:
  - trusted operator input
  - assistant Result brief
  - WorkItem objective, plan, todo, wait, or completion transition
  - verification outcome
  - PR, issue, review, CI, or external state transition
  - task failure or success that changes next action

normal:
  - useful task result
  - tool execution summary
  - external event with actionable content

low:
  - duplicate external wake
  - scheduler tick with no state change
  - repeated pending poll
  - ack-only or bookkeeping turn

diagnostic:
  - provider fallback
  - provider retry
  - transport recovery with no semantic state change
```

These classes should be derived from runtime-known structure and deltas. If the
runtime is unsure, it should classify conservatively as normal or high rather
than dropping the turn.

### 5.5 Preserve Authority Boundaries Across Compression

LLM-generated summaries may help explain old turns, but they must not become
the authority source for:

- whether input is trusted operator input
- whether a WorkItem objective changed
- whether an external event can change scope
- whether a task result overrides operator intent
- whether a turn may be discarded despite trusted or state-changing content

Compressed turn episodes should carry provenance:

```text
compressed_turn_episode:
  covered_turn_range
  source_turn_ids
  source_refs
  generated_by: model
  operator_intents
  decisions
  results
  verification
  unresolved_items
  model_inferences
```

The summary is evidence for the model, not a replacement for the runtime
ledger.

### 5.6 Project Recent Turns By Chain And Priority, Not FIFO Alone

Prompt assembly should not simply render the last N physical turns.

Instead, it should always include:

1. the current turn
2. the continuation anchor turn or current WorkItem projection
3. the latest trusted operator intent relevant to the current chain
4. the latest user-facing result for that chain, when present
5. the latest wait, completion, verification, or state transition relevant to
   the chain

Then, within budget, include:

- recent intent-bearing turns
- recent result-bearing turns
- recent state-transition turns
- current continuation-chain turns

Low and diagnostic turns should be folded unless they are directly relevant to
debugging the current failure.

Example prompt shape:

```text
current_turn:
  id: turn_180
  trigger: task_result
  relation: continues_operator_intent

continuation_anchor:
  operator_turn_id: turn_150
  work_item_id: wi_123

current_chain_turns:
  - turn_150:
      type: operator_intent
      input_summary: fix the PR review comments
  - turn_160:
      type: task_result
      result_summary: cargo test failed in prompt snapshots
  - turn_170:
      type: assistant_result
      result_summary: updated snapshots and restarted tests
  - turn_180:
      type: task_result
      result_summary: cargo test passed

folded_runtime_turns:
  - turns: turn_156-turn_159
    summary: provider fallback and retry turns; no semantic state change
```

### 5.7 Use Budgets, Not One Global Recent-Turn Count

The user-facing mental model should be "recent semantic turns for the current
continuation", not "the last N runtime activations".

The turn projection budget should be derived from the resolved prompt
projection budget, not from a fixed message count or a fixed physical-turn
count. A first implementation should use a simple proportional budget with a
floor and ceiling:

```text
turn_projection_budget = clamp(
  prompt_budget_estimated_tokens * 0.30,
  min = 4096,
  max = 64000,
)
```

For a model with a resolved prompt budget of `258400` estimated tokens, this
would allocate `64000` estimated tokens to turn projection after applying the
ceiling. For a small fallback model or an unresolved default policy, the floor
keeps recent semantic turns from collapsing below `4096` estimated tokens.

The budget should be spent by retention priority, continuation-chain
membership, and semantic turn value, not by physical-turn FIFO order. Current
input and pinned turn anchors are mandatory and may borrow from the free pool
before lower-priority history is included.

Configuration can still use bounded selection limits in addition to the token
budget, for example:

```text
turn_projection:
  token_budget_ratio: 0.30
  min_token_budget: 4096
  max_token_budget: 64000
  max_physical_turns_considered: 64
  min_intent_turns: 3
  min_result_turns: 3
  max_current_chain_turns: 12
  max_global_recent_semantic_turns: 8
  fold_low_priority_after: 2
```

These values are first-pass defaults and limits, not a replacement for
priority-based selection. The contract is that pinned and high-priority causal
turns should not be evicted by low-information runtime activations.

Later implementations may tune the ratio by continuation type, for example:

```text
operator new intent:         20% - 30%
operator continuation:       30% - 35%
task/external continuation:  35% - 40%
debug/diagnostic turn:       20% plus targeted refs
```

The first implementation should prefer one stable default ratio before adding
trigger-sensitive tuning.

## 6. Runtime Versus LLM Responsibilities

The runtime is responsible for:

- trigger kind
- provenance and trust
- turn ids and source refs
- continuation-chain linkage
- WorkItem, wait, completion, task, and tool deltas
- baseline retention priority
- budget-aware prompt selection

An LLM may be used for:

- natural language summaries of older turn ranges
- decision and open-question extraction
- verification or failure explanation
- optional retention hints that can only raise priority within runtime policy

An LLM must not be the sole judge of authority, trust, or source replacement.

## 7. Migration Path

### Phase 1: Turn Projection Without New Storage

Build an in-memory turn projection from existing message, brief, task, tool,
and WorkItem ledgers during prompt assembly.

This phase can keep current prompt sections as compatibility fallback while
adding a `recent_turns` or `turn_projection` section.

### Phase 2: Durable Turn Linkage

Persist turn ids and source refs on newly produced briefs, task results, tool
executions, wait transitions, and WorkItem transitions.

This makes prompt assembly less heuristic and improves auditability.

### Phase 3: Priority-Based Selection And Folding

Replace fixed recent-message and recent-brief windows as the primary context
surface with priority-aware turn selection.

Low and diagnostic turns should be folded into compact runtime summaries.

### Phase 4: Structured Episode Compaction

When turn ranges age out of prompt budget, compact them into structured
episodes that preserve source refs, authority boundaries, and unresolved
items.

## 8. Acceptance Scenarios

The design is acceptable when these scenarios are supported:

1. A task-result wake after a trusted operator request renders the task result
   as continuation input and keeps the operator-intent turn visible or
   referenced.
2. A provider fallback or retry turn does not evict the latest trusted operator
   intent from the prompt.
3. A result brief is rendered together with, or referenced back to, the turn
   and input it answered.
4. Repeated no-op scheduler ticks or duplicate external wakes are folded and
   do not consume the main recent-turn budget.
5. Current WorkItem objective, plan, todo, wait, and completion transitions
   remain higher-authority context than model-generated summaries.
6. A compressed older episode preserves source turn ids and separates operator
   intent, runtime facts, task results, and model inference.
7. Under tight prompt budget, pinned and high-priority turns survive before
   low or diagnostic turns.

## 9. Proposed Decisions And Remaining Questions

This section records the current working answers to the design questions above.
They should be treated as proposed decisions for the first implementation, not
as final storage schema.

### 9.1 Minimal Durable Turn Linkage

Proposed decision: persist turn ownership refs before replacing parallel
`recent_messages` and `recent_briefs` as the primary prompt surface.

The first durable boundary does not need a complete normalized turn-storage
schema, but every cross-turn-visible object should be traceable to the turn
that produced or triggered it:

```text
operator input -> turn
turn -> assistant brief/result
turn -> task/tool/external refs
turn -> WorkItem transition
task/external wake turn -> continuation anchor turn or WorkItem
```

The minimum durable linkage is:

- every turn has a stable `turn_id`
- every message, brief, task result, tool execution, wait transition, and
  WorkItem transition visible across turns records its producing or triggering
  `turn_id`
- task-result, external-event, and timer wake turns can point back to a
  continuation anchor, at least as an anchor turn, trusted operator-intent
  message, or WorkItem id

Remaining question: how much bounded summary text should be persisted on the
turn record itself versus kept only on the referenced object?

### 9.2 Continuation Chains

Proposed decision: derive continuation chains from existing WorkItem, task,
wait, trigger, and anchor refs first; materialize chain ids later as an index
or cache once the rules stabilize.

The first implementation should not make `continuation_chain_id` an
authoritative source of truth. It can be derived by rules such as:

```text
if current WorkItem exists:
  chain = work_item_id
else if task result:
  chain = task.spawned_by_turn.continuation_anchor
else if external wake matched wait:
  chain = wait.work_item_id or wait.anchor_turn_id
else if operator input:
  chain = current turn unless classified as refinement or override
else:
  chain = previous active continuation anchor
```

If a materialized chain field is added later, it should remain a projection
hint with provenance such as `derived_from`, not the only durable source of
causal truth.

Remaining question: which operator follow-ups should automatically continue
the previous chain, and which should start a new chain, when no WorkItem exists?

### 9.3 Settlement-Time Deltas Versus Prompt-Time Projection

Proposed decision: record factual, authoritative, recovery-relevant deltas at
turn settlement time; derive display projection, folding, ordering, and budget
selection during prompt assembly.

Settlement should record runtime-known facts:

- `turn_id`
- trigger kind and trigger refs
- input message refs
- task result refs
- external event refs
- tool execution refs
- produced brief and latest-result refs
- WorkItem objective, plan, todo, wait, and completion deltas
- `WaitFor` and `CompleteWorkItem` transitions
- started and completed task ids
- provider attempt, retry, fallback, and recovery refs
- exit outcome: result, wait, complete, error, or interrupted
- continuation anchor used by the turn

Prompt assembly may derive:

- retention priority
- current-chain turn selection
- semantic turn grouping
- folded runtime-turn summaries
- token-budget trimming
- episode grouping and natural-language recap

Retention priority may later be cached with a `computed_by_version`, but the
first implementation should avoid permanently deciding at settlement time that
a turn is unimportant.

Remaining question: which derived projection fields are worth caching for
debuggability or performance once the prompt projection rules settle?

### 9.4 Operator-Facing Transcript

Proposed decision: render semantic turns by default and expose folded physical
turns through diagnostics and source refs.

The normal operator-facing view should not show every provider retry, duplicate
wake, no-op scheduler tick, or repeated pending poll as an equal transcript
turn. Those physical turns should fold under the semantic turn or continuation
they relate to:

```text
semantic_projection:
  display_turn_id
  primary_physical_turn_id
  related_physical_turn_ids
  folded_turn_ranges
  source_refs
```

The audit/debug view must still be able to expand folded physical turns and
show their original trigger, refs, and outcome.

Remaining question: what UI or CLI affordance should expose the expanded
physical-turn diagnostics without making the default transcript noisy?

### 9.5 Model-Generated Retention Hints

Proposed decision: defer model-generated retention hints until rule-based turn
projection is stable.

The first implementation should rely on runtime facts for baseline retention:

- pinned: current turn, continuation anchor, latest trusted operator intent,
  current WorkItem source or transition
- high: trusted operator input, user-facing result, WorkItem/wait/completion
  delta, verification outcome, task terminal outcome, PR/review/CI state
  change
- low or diagnostic: provider retry, fallback, no-op tick, duplicate wake,
  repeated pending poll, ack-only bookkeeping

LLMs may generate episode summaries and natural-language recaps, but those
summaries are evidence for the next model call, not authority for trust,
operator intent, WorkItem state, or discard decisions.

If model retention hints are added later, they should be non-authoritative:

- they may raise a turn's priority within runtime policy
- they must not lower or discard trusted operator, WorkItem, wait/completion,
  or state-transition turns
- they must carry source turn ids and confidence or reason metadata
- they must be possible to disable for debugging

Remaining question: after the rule-based projection is stable, which classes
of semantic ambiguity are worth asking a model to classify?

### 9.6 Turn Projection Token Budget

Proposed decision: derive `turn_projection_budget` from the resolved prompt
projection budget using a ratio plus minimum and maximum bounds:

```text
turn_projection_budget = clamp(
  prompt_budget_estimated_tokens * 0.30,
  min = 4096,
  max = 64000,
)
```

The initial policy should allocate around 30% of the prompt budget to semantic
turn projection, while guaranteeing a usable floor for fallback contexts and
preventing very large context windows from spending unbounded tokens on turn
history.

This budget should be consumed by priority:

- pinned turns: current turn, current input, anchor trusted operator intent,
  current WorkItem transition, latest user-facing result, and active
  wait/completion deltas
- high-priority turns: trusted operator input, assistant result brief,
  verification result, task terminal result, design decision, and explicit
  operator correction or override
- folded or compressed turns: provider fallback, retry, duplicate wake,
  scheduler tick, pending poll, no-op turn, and large diagnostic output

The first implementation should not ask "how many recent turns should be
kept?" as the primary contract. It should ask "how much turn-projection budget
is available?" and then fill that budget with pinned, chain-relevant, and
high-value semantic turns before lower-value physical turns.

Remaining question: after the baseline ratio is implemented, should the ratio
be adjusted by trigger class, continuation class, or observed prompt pressure,
and where should those tuning rules live?
