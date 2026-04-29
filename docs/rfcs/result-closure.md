---
title: RFC: Result Closure
date: 2026-04-21
status: accepted
issue:
  - 44
---

# RFC: Result Closure

## Summary

Holon should model closure with three orthogonal concepts:

- `closure_outcome`: `completed`, `continuable`, `failed`, or `waiting`
- `waiting_reason`: present only when `closure_outcome = waiting`
- `runtime_posture`: whether execution is currently active or suspended

This keeps business meaning separate from runtime posture. `sleeping` is not a
result. It is one possible runtime posture after a result has already been
derived.

## Why

Before this RFC, Holon's "done / still waiting / failed" semantics were spread
across prompt wording, text output, `Sleep`, task state, and follow-up
behavior. That made `run` and `serve` harder to explain consistently and left
operators without a stable answer to "what is the agent waiting on right now?"

## Core Model

### Closure Outcome

Holon should derive one outcome for the current unit of work:

- `completed`
  - the current unit is closed successfully
- `continuable`
  - the current execution pass is closed, but runnable work remains and the
    runtime may self-reactivate without waiting on a new external condition
- `failed`
  - the current unit is closed in explicit failure
- `waiting`
  - the current execution pass is closed, but useful progress depends on a
    future trigger

### Waiting Reason

When `closure_outcome = waiting`, Holon should also record one reason:

- `awaiting_operator_input`
- `awaiting_external_change`
- `awaiting_task_result`
- `awaiting_timer`

### Runtime Posture

Runtime posture is separate from closure semantics. A runtime may be:

- actively executing
- idle but runnable
- sleeping / suspended

Examples:

- `waiting + awaiting_external_change + sleeping`
- `waiting + awaiting_task_result + sleeping`
- `continuable + awake`
- `completed + idle`

## Source Of Truth

The runtime is the final arbiter of closure. The model may provide hints, but
Holon should not infer operator-visible state from assistant prose alone.

Rules:

- assistant text is evidence, not authority
- tool and task state outrank free-form completion claims
- explicit runtime primitives outrank phrasing such as "I am waiting for..."
- the runtime may reject a model completion claim when blocking evidence still
  exists

## Evidence-Driven Derivation

Closure should be derived from explicit runtime evidence, in this order:

1. terminal runtime or task failure
2. explicit blocking wait state with strong runtime evidence
3. active blocking task with no remaining local progress
4. explicit timer wait
5. explicit external wait or condition wait
6. runnable work-item continuation with no higher-priority blockers
7. successful completion evidence with no remaining blockers

This keeps long-running execution honest. Holon should not claim success only
because the assistant stopped talking.

## Semantics By Runtime Layer

### Turn

A turn settles after one execution pass. The turn outcome is the immediate
operator-visible explanation for that pass.

### Task

A task closes independently of the parent turn. Task completion or failure can
feed back into a waiting parent, but a created task does not automatically mean
the parent is now `awaiting_task_result`.

### Agent

Agent-level state reflects the latest settled work posture:

- active work still running
- closed and continuable with runnable queued or active work
- closed and idle
- closed and waiting with an explicit reason
- failed and requiring recovery

## `run` Mapping

For `holon run`, the terminal status should reflect the derived closure result:

- `completed` returns final assistant text when present
- `continuable` reports unfinished runnable work as waiting-like instead of
  synthesizing a false success
- `failed` surfaces a failure artifact
- `waiting` reports waiting explicitly instead of synthesizing a false success

## Invariants

- `sleeping` is never a closure outcome
- `waiting_reason` is never present when `closure_outcome != waiting`
- `continuable` must not claim an external wait reason by itself
- one closure result must be derivable without parsing assistant prose
- the runtime may stay awake after a `waiting` result, but it must still record
  the waiting reason explicitly

## Related Historical Notes

Supersedes and absorbs:

- `docs/archive/result-closure-contract.md`
- `docs/archive/evidence-driven-closure.md`
