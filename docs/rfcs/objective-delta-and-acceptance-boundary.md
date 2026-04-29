---
title: RFC: Objective, Delta, and Acceptance Boundary
date: 2026-04-21
status: accepted
issue:
  - 45
---

# RFC: Objective, Delta, and Acceptance Boundary

## Summary

Holon should persist explicit high-level work state instead of relying on
assistant phrasing alone. The minimum state is:

- `current_objective`
- `last_delta`
- `acceptance_boundary`

This state should survive follow-up turns, waiting, delegation, and compaction.

## Why

Without explicit objective state, follow-ups tend to drift. The runtime cannot
reliably tell whether a new message continues the same task, narrows it,
replaces it, or starts a second piece of work.

## Model

### Current Objective

`current_objective` is the best runtime representation of the work currently
being advanced.

It should answer:

- what Holon is trying to achieve now
- which work item or active turn this is attached to
- what "still on task" means for follow-up handling

### Delta

`last_delta` captures how the objective most recently changed, such as:

- continued
- narrowed
- replaced
- appended
- resumed after waiting

This is a state transition record, not a second objective.

### Acceptance Boundary

`acceptance_boundary` records what would count as done for the current
objective. It should stay operator-visible enough to support honest closure and
verification reporting.

## Sources Of Truth

The runtime owns the persisted fields. The model may propose updates, but the
runtime decides what transition actually happened based on ingress type and
current state.

## Follow-Up Classification

New input should classify into one of four primary transitions:

- continue the current objective
- narrow the current objective
- replace the current objective
- append additional work

The transition should be explicit so resume and compaction do not have to infer
intent from transcript fragments later.

## Delegation And Rejoin

Delegated work and task results should attach back to the objective that
created them. Rejoin behavior should not erase the active objective unless the
runtime explicitly replaces it.

## Compaction And Resume

Compaction should preserve:

- the current objective
- the current acceptance boundary
- the latest meaningful delta

These are first-class runtime facts, not optional prose summaries.

## Invariants

- the active objective must always be explainable without replaying the whole
  transcript
- acceptance state must not be inferred solely from final assistant text
- follow-up classification must update objective state explicitly

## Related Historical Notes

Supersedes and absorbs:

- `docs/archive/objective-delta-and-acceptance-boundary.md`
