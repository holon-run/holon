---
title: RFC: Continuation Trigger
date: 2026-04-21
status: accepted
issue:
  - 48
---

# RFC: Continuation Trigger

## Summary

Holon should classify continuation triggers by whether they:

- only affect liveness
- produce model-visible continuation input
- update waiting state or work state without immediately creating a new
  conversational pass

This RFC defines one trigger vocabulary for `run` and `serve`.

## Trigger Types

Phase-1 trigger types:

- operator follow-up
- task result
- timer / scheduled wake
- contentful external event
- wake hint
- runtime-owned internal follow-up

## Core Judgments

### Wake hints are not contentful messages

A wake hint means "something changed; reconsider whether work should resume."
It should not automatically enter model context as message content.

### Contentful external events are queue items

External input that carries meaningful payload should become a queue item with
provenance and trust metadata.

### Waiting reason constrains valid continuation

Not every trigger should satisfy every waiting state. The continuation model
should respect the reason the runtime was waiting in the first place.

## Trigger Classes

### Liveness-Only

These may reactivate scheduling without automatically injecting content:

- wake hint
- some timer ticks
- internal follow-up used only to re-evaluate runtime state

### Model-Visible Continuation

These become queued continuation input:

- operator follow-up
- task result
- contentful external event
- timer events that carry contentful resume instructions

## Waiting Matrix

### `awaiting_operator_input`

Primary satisfying trigger:

- operator follow-up

Other triggers may wake bookkeeping, but they must not silently satisfy the
 operator-input boundary.

### `awaiting_task_result`

Primary satisfying trigger:

- task result

Operator follow-up may redirect work, but it should be modeled as a new control
decision, not as if the task result had arrived.

### `awaiting_timer`

Primary satisfying trigger:

- matching timer wake

Operator follow-up may override the wait by changing the objective or work
queue.

### `awaiting_external_change`

Primary satisfying triggers:

- contentful external event
- wake hint tied to an existing condition or subscription

Wake hints only re-evaluate liveness; they do not by themselves create model
content.

## Queue Rule

All contentful continuation should still pass through the main queue. Holon
should not create a bypass path for task results, callbacks, or operator
follow-ups.

## Mismatched Triggers

When a trigger does not satisfy the current waiting reason, Holon should do one
of three explicit things:

- keep waiting and record the ignored wake
- enqueue the event as separate follow-on work
- replace or redirect the current objective through normal work-state updates

It should not silently pretend the waiting reason was satisfied.

## Related Historical Notes

Supersedes and absorbs:

- `docs/archive/continuation-trigger-contract.md`
- `docs/archive/triggering-and-liveness.md`
- `docs/archive/wake-hint-vs-contentful-event.md`
- `docs/archive/condition-subscription-and-event-wake.md`
