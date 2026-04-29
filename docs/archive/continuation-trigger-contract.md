# Continuation Trigger Contract

Date: 2026-04-08

Related issue:

- `holon-run/holon#48` `RFC: continuation trigger contract`

Related RFC:

- [docs/result-closure-contract.md](result-closure-contract.md)

This RFC defines how `Holon` continues after a unit of work has already
reached a closure decision.

The focus is not "what is the closure state?" That is answered by the closure
RFC. The focus here is:

- what counts as a continuation trigger
- which triggers only affect liveness
- which triggers produce model-visible continuation
- how a trigger relates to the prior waiting reason

## Problem

Today, continuation can be initiated through several different paths:

- operator follow-up
- task result
- callback or wake hint
- timer fire
- `InternalFollowup`
- runtime-owned `SystemTick`

Those paths already exist, but they are still understood mostly as queue and
liveness mechanics. They are not yet governed by one continuation contract.

This creates several problems:

- wake and continuation are easy to blur together
- the same trigger can be interpreted differently in `run` and `serve`
- mismatched triggers do not have a clear contract
- operator-visible waiting state and actual resumption behavior can drift apart

## Goal

This RFC answers four questions:

1. What kinds of continuation triggers exist?
2. Which triggers are model-visible?
3. Which triggers are valid continuations for each waiting reason?
4. When should a trigger wake the runtime without necessarily creating a new
   meaningful continuation turn?

## Non-Goals

This RFC does not define:

- objective / delta / acceptance-boundary semantics
- approval UX
- prompt wording
- proactive scheduler policy
- long-term `awaiting_operator_input` primitive design

Those may depend on this contract, but they should not define it.

## Core Judgments

### 1. Continuation happens after closure, not instead of closure

`Holon` should treat continuation as a state transition that begins from a
prior closure decision.

That means:

- first determine `closure_outcome`
- then determine whether a new trigger is valid continuation for that state

The runtime should not decide continuation in isolation from closure state.

### 2. Wake and continuation should stay distinct

Some triggers only affect runtime liveness:

- waking a sleeping runtime
- coalescing a pending wake hint
- preserving auditability and delivery

That is not yet the same thing as meaningful continuation.

So `Holon` should distinguish:

- wake: the runtime becomes eligible to run again
- continuation: the runtime has a legitimate reason to re-enter model-visible
  work

### 3. Triggers should be typed by source, not inferred from free-form text

The runtime should model continuation triggers as explicit runtime-visible
types, not prompt-parsed intent.

Examples:

- operator input
- task result
- timer fire
- external contentful event
- explicit internal follow-up
- runtime-owned system tick

This keeps continuation explainable and auditable.

### 4. Waiting reason and continuation trigger should usually match

The prior waiting reason is the strongest guide for what kind of continuation
is expected next.

Examples:

- `awaiting_task_result` is best resumed by `task_result`
- `awaiting_timer` is best resumed by `timer_fire`
- `awaiting_operator_input` is best resumed by `operator_input`

But this is guidance, not an absolute prohibition.

Some triggers may still legitimately continue the agent even if they do not
match the previous waiting reason:

- operator input can always redirect or override prior waiting
- an explicit internal follow-up can continue local work without a waiting
  state
- a contentful external event may supersede a prior external wait

So the runtime should model:

- expected trigger
- allowed trigger
- mismatched but still valid override

## Trigger Types

The continuation contract should recognize at least these trigger types:

- `operator_input`
- `task_result`
- `external_event`
- `timer_fire`
- `internal_followup`
- `system_tick`

Meaning:

- `operator_input`
  - a new operator-authored message or control-plane follow-up
- `task_result`
  - a delegated task or child execution produced a terminal result
- `external_event`
  - a contentful outside signal arrived through callback, inbox, webhook, or
    wake-capable ingress
- `timer_fire`
  - a scheduled timer reached its fire point
- `internal_followup`
  - the agent or a tool explicitly queued bounded same-agent continuation
- `system_tick`
  - the runtime decided the agent deserves reconsideration now

## Trigger Classes

Each trigger type should be interpreted through one of these classes:

- `resume_expected_wait`
- `resume_override`
- `local_continuation`
- `liveness_only`

Meaning:

- `resume_expected_wait`
  - the trigger matches the prior waiting reason
- `resume_override`
  - the trigger intentionally supersedes the prior waiting reason
- `local_continuation`
  - the trigger continues local work without needing a prior waiting state
- `liveness_only`
  - the trigger changes runtime eligibility without by itself justifying a new
    meaningful continuation turn

## Waiting-Reason Matrix

The runtime should use this baseline matrix.

### If prior state is `waiting + awaiting_operator_input`

- expected:
  - `operator_input`
- allowed override:
  - `operator_input`
  - possibly `system_tick` only if runtime policy explicitly transforms the
    state first
- not expected:
  - `task_result`
  - `timer_fire`
  - generic `external_event`

Default interpretation:

- ordinary external signals should not satisfy operator wait
- task completion should remain observable, but should not silently replace the
  need for operator input

### If prior state is `waiting + awaiting_task_result`

- expected:
  - `task_result`
- allowed override:
  - `operator_input`
- not expected:
  - generic `external_event`
  - `timer_fire`

Default interpretation:

- terminal delegated work should rejoin the parent
- operator input may redirect the agent before the task finishes

### If prior state is `waiting + awaiting_external_change`

- expected:
  - `external_event`
- allowed override:
  - `operator_input`
  - `system_tick` if runtime policy says re-evaluate now
- not expected:
  - `task_result` unless the wait state changed first

Default interpretation:

- contentful external events are the main continuation path
- wake-only signals should not automatically imply meaningful continuation

### If prior state is `waiting + awaiting_timer`

- expected:
  - `timer_fire`
- allowed override:
  - `operator_input`
- not expected:
  - generic external event

Default interpretation:

- the timer is the canonical resume trigger
- operator intervention may still override the wait

### If prior state is `completed` or `failed`

- allowed:
  - `operator_input`
  - `internal_followup`
  - `external_event` if routing policy targets this agent
  - `system_tick` only if runtime policy explicitly allows it

Default interpretation:

- these triggers begin a new continuation pass
- they do not mutate the prior closure record retroactively

## Trigger Semantics

### `operator_input`

`operator_input` is always model-visible.

It can:

- resume an expected operator wait
- override prior waiting state
- begin a new pass after completion or failure

This is the strongest explicit continuation trigger because it carries fresh
human intent.

### `task_result`

`task_result` should become model-visible continuation when:

- the parent is waiting on blocking delegated work
- or runtime policy says task rejoin is meaningful now

Non-terminal task status updates should remain observable, but should not be
treated as the canonical continuation trigger. The canonical rejoin point is
the terminal task result.

### `external_event`

`external_event` should be model-visible continuation only when the event is
contentful or routing policy explicitly marks it wake-capable.

This is different from a bare wake signal.

So `Holon` should continue distinguishing:

- wake-only external signals
- contentful external events

### `timer_fire`

`timer_fire` is model-visible continuation for timer-based waits.

It should be treated as:

- explicit resumption of `awaiting_timer`
- not just another generic wake reason

### `internal_followup`

`internal_followup` is explicit same-agent continuation.

It is valid even without a prior waiting state.

This is the preferred mechanism for:

- bounded self-stitching
- tool-driven local follow-up
- explicit next-step queuing

### `system_tick`

`system_tick` is runtime-owned and should remain distinct from model-owned
continuation.

By default it should be treated conservatively:

- it may wake the runtime
- it may cause reconsideration
- but it should not by itself replace explicit operator, task, timer, or
  contentful external triggers unless scheduler policy explicitly allows it

## Mismatched Triggers

When a trigger does not match the prior waiting reason, the runtime should not
silently pretend it matched.

Instead it should record that the continuation was:

- expected
- override
- or liveness-only

Examples:

- `awaiting_task_result` + `operator_input`
  - valid override
- `awaiting_operator_input` + generic `external_event`
  - not a valid replacement by default
- `awaiting_external_change` + wake-only signal with no content
  - liveness-only by default

This should be auditable through runtime events.

## Queue Rule

All continuation triggers should still re-enter through the main queue.

That preserves:

- origin
- trust
- auditability
- scheduling consistency
- one runtime-owned continuation path

So no trigger should bypass the queue and directly execute tools or model work.

## Implementation Direction

Phase 1 should add a runtime-visible continuation trigger model without
replacing the whole runtime loop.

Minimum expectations:

- add typed continuation-trigger vocabulary
- map current message kinds and wake paths into that vocabulary
- distinguish wake-only from model-visible continuation
- record continuation decisions in audit events
- use prior `ClosureDecision` as the semantic starting point

Phase 1 does not need to solve:

- full objective replacement semantics
- proactive scheduling policy
- complete `awaiting_operator_input` signal design

## Invariants

1. Continuation must start from a prior closure state.
2. Wake and continuation are not the same thing.
3. Triggers should be typed by runtime-visible source, not inferred from free
   text.
4. A mismatched trigger should be recorded as mismatch or override, not silently
   treated as expected.
5. All continuation triggers should re-enter through the main queue.
6. `system_tick` remains runtime-owned.

## Decision

`Holon` should define continuation as a typed transition from a prior closure
decision, with explicit trigger kinds, expected waiting-reason matches, and a
clear distinction between wake, override, and model-visible continuation.
