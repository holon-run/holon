---
title: RFC: Context Candidate Budget Planner
date: 2026-07-17
status: accepted
issue:
  - 2268
---

# Context Candidate Budget Planner

This RFC defines the selection boundary between runtime context projection and
final prompt rendering.

It extends:

- [Continuation Anchor](./continuation-anchor.md)
- [Turn-Based Context Projection](./turn-based-context-projection.md)
- [Debug Prompt JSON Envelope](./debug-prompt-json-envelope.md)

## 1. Problem

Context assembly previously rendered and appended sections while decrementing a
shared budget. The append order therefore acted as an implicit priority and
drop policy. Only `current_input` had a separate reservation, so focused
WorkItem truth and a trusted continuation anchor could be displaced by sections
that happened to be rendered first.

The final section list also preserved only the selected result. It did not
record why a considered section was kept, compacted, truncated, or omitted.

## 2. Goals

- Separate candidate collection, budget selection, and final render order.
- Make pinned state, retention priority, drop tier, and render order explicit.
- Produce the same plan for the same candidates regardless of collection order.
- Keep `current_input`, focused `current_work_item`, and
  `continuation_anchor` at a defined minimum representation when they exist.
- Record typed budget decisions for every considered candidate.
- Preserve the existing total context hard cap and provider wire protocol.

## 3. Non-Goals

- Do not replace all context domain payloads with one large enum.
- Do not change the recent-turn semantic selector or reduce it to string FIFO.
- Do not define the complete debug prompt JSON envelope.
- Do not unify context, system, tool, and turn-local continuation budgets.
- Do not make every diagnostic section pinned.

## 4. Candidate Contract

A `ContextCandidate` is a section-level typed envelope. It contains:

- a stable candidate id, shared with the final section id
- the section stability and rendered full representation
- an optional compact representation
- a policy containing:
  - `pinned`
  - retention priority
  - drop tier
  - render order

Candidate ids must be non-empty and unique. A compact representation must keep
the same id as its full representation.

Pinned candidates must provide a compact representation. The compact form is
the minimum truthful representation, not an arbitrary substring selected after
other sections consume the budget.

## 5. Initial Pinned Set

The following candidates are pinned when present:

1. `current_input`
2. focused `current_work_item`
3. `continuation_anchor`

Their compact forms preserve:

- the current input provenance/header and a bounded input body
- the focused WorkItem id and objective
- the trusted operator relation or bounded continuation anchor

Absence is not synthesized as pinned truth. For example, the explicit
`current_work_item: none` section remains a normal candidate.

## 6. Deterministic Planning

Planning follows these phases:

1. validate ids, compact ids, and pinned compact availability
2. calculate the total pinned minimum
3. reserve every pinned compact representation
4. allocate remaining budget by explicit selection keys
5. emit final sections using independent render-order keys

Selection keys are:

1. pinned before non-pinned
2. higher retention priority first
3. later-drop tiers before earlier-drop tiers
4. stable candidate id as the final tie-break

Collection or insertion order is never a tie-break.

Render keys are:

1. explicit render order
2. stable candidate id

This keeps model-facing section layout independent from retention priority.

## 7. Outcomes And Evidence

Each candidate produces one `ContextPlanDecision` containing:

- candidate id and section name
- requested estimated tokens
- minimum estimated tokens
- allocated estimated tokens
- outcome
- typed reason code

Initial outcomes are:

- `full`
- `compact`
- `truncated`
- `omitted`

Initial reason codes include:

- `selected_full`
- `selected_compact_for_pinned_minimum`
- `selected_compact_for_budget`
- `truncated_to_remaining_budget`
- `omitted_lower_priority`
- `omitted_drop_tier`

Decision evidence is runtime-internal prompt metadata. The human debug prompt
dump displays it, but this RFC does not change provider request lowering or
stabilize the draft JSON debug envelope.

## 8. Over-Budget Failure

If the sum of pinned compact representations exceeds the total context budget,
planning fails closed with `pinned_minimum_over_budget`.

The diagnostic includes:

- total budget
- required pinned minimum

The planner must not silently omit a pinned candidate or exceed the hard cap.
This is the narrow Context build error boundary; it does not introduce a global
runtime error taxonomy.

## 9. Recent-Turn Boundary

`recent_turns` remains a semantic projection built from turn records and linked
runtime evidence. Its full and compact candidates are produced by rerunning the
existing semantic projection with different budgets.

Turn-local recovery may still reproject `recent_turns` with a smaller budget.
That recovery must rebuild the rendered prompt and update the corresponding
planning evidence rather than truncating the section as an opaque string.

## 10. Compatibility

- Final context remains within `prompt_budget_estimated_tokens`.
- `PromptStability` remains attached to the selected representation.
- Prompt cache fingerprints continue to derive from final selected sections.
- Provider request wire shapes do not change.
- Existing section ids remain stable.

Tests must cover pinned minimum boundaries, order independence, selection versus
render ordering, all outcomes, debug evidence, recent-turn reprojection, and
the total hard cap.
