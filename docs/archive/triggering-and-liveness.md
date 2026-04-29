# Triggering And Liveness Direction

This note summarizes Holon's current liveness model and clarifies the intended
roles of `Sleep`, `InternalFollowup`, and `SystemTick`.

The core question is:

- how does an agent remain alive across time without degenerating into either
  one-shot request/response execution or uncontrolled self-looping?

## Current Implemented Model

Holon already has a real long-lived runtime shape.

The current design is:

- one runtime loop per session
- one normalized queue for all wake triggers
- explicit `Awake*`, `AwaitingTask`, and `Asleep` agent states
- wake triggered by queued work, not by recursive prompt tricks

This means Holon is already more than a single LLM call wrapper.

### What Currently Wakes A Session

At the moment, an agent can continue running because of:

- operator input
- HTTP enqueue / external ingress
- timer ticks
- background task status and result rejoin
- internal follow-up messages

All of these re-enter through the same queue model.

### What Currently Puts A Session To Sleep

There are currently two sleep paths.

#### 1. Runtime-driven sleep

This is the default and most important one.

If the queue is empty, the runtime transitions the agent to `Asleep`.

This is the real liveness backbone today.

#### 2. Model-requested sleep

The model can call the `Sleep` tool.

That does not literally block the runtime. Instead it marks the current loop as
complete and asks the runtime to transition to sleep after result delivery.

So sleep is currently:

- finalized by the runtime
- optionally requested earlier by the model

## Current Limitation

Holon already has `SystemTick` in the message model, but it does not yet have a
real proactive tick loop that injects `SystemTick` messages as part of runtime
policy.

So the current model is better described as:

- event-driven continuation

Not yet:

- proactive runtime-owned reconsideration

This distinction matters.

## Three Different Concepts

These three primitives should stay distinct.

### `Sleep`

`Sleep` is a model-owned intent.

It means:

- "there is no useful work to continue right now"

Potential future shape:

- `Sleep()`
- `Sleep(duration_ms)`

But the important point is that this is still the model expressing intent, not
the runtime seizing control.

### `InternalFollowup`

`InternalFollowup` is a model- or tool-triggered continuation inside the same
agent.

It means:

- "enqueue another explicit step for this agent"

This is useful for bounded self-continuation and workflow stitching.

But it is still not the same thing as a scheduler-owned tick.

### `SystemTick`

`SystemTick` should be a runtime-owned scheduling primitive.

It means:

- "the runtime has decided it is worth reconsidering this agent now"

This is not just another message kind. It is the boundary between:

- model intent
- runtime scheduling policy

## Why `SystemTick` Should Not Be LLM-Callable

`SystemTick` should not be exposed as a tool the model can call directly.

The reason is simple:

- if the model can emit scheduler ticks directly, it can keep itself alive
  indefinitely
- this moves control from runtime policy into prompt behavior
- it becomes harder to reason about boundedness, backoff, and resource use

The intended separation is:

- the model may request `Sleep`
- the model may enqueue explicit follow-up work
- the runtime alone decides whether to inject `SystemTick`

So:

- `Sleep` is LLM-owned intent
- `SystemTick` is runtime-owned scheduling

## Should `Sleep` Take A Time Parameter?

Yes, probably.

But it should not replace `SystemTick`.

They solve different problems:

- `Sleep(duration_ms)` means "wake me after this delay"
- `SystemTick` means "the runtime decided this agent deserves another look"

So a future `Sleep(duration_ms)` would best be understood as:

- syntax sugar for "schedule a timer, then sleep"

Not as:

- the universal continuation mechanism

## Suggested Direction

Holon should move toward this model.

### 1. Keep runtime-owned automatic sleep ✓ IMPLEMENTED

If no work remains, the runtime should be able to sleep even if the model never
calls `Sleep`.

This is the safe default.

**Implementation status:** The runtime automatically transitions to `Asleep` when the queue is empty and no active work remains.

### 2. Let `Sleep` support an optional delay

Future shape:

- `Sleep()`
- `Sleep(duration_ms)`

But the runtime should still enforce policy:

- minimum delay
- maximum delay
- possibly mode-specific limits

**Implementation status:** `Sleep` currently accepts only a reason string. The `sleep_job` task kind exists for delayed work, but `Sleep` itself does not yet take a duration parameter.

### 3. Keep `InternalFollowup` as explicit same-agent continuation ✓ IMPLEMENTED

This remains the mechanism for:

- tool-driven re-entry
- explicit local continuation
- bounded self-stitching inside one session

**Implementation status:** `InternalFollowup` is implemented as a message kind. The `Enqueue` tool creates `InternalFollowup` messages for explicit same-agent continuation.

### 4. Reserve `SystemTick` for the runtime ✓ IMPLEMENTED

`SystemTick` should only be emitted by the runtime or scheduler layer.

It should not be available as a normal model tool.

**Implementation status:** `SystemTick` is runtime-owned and emitted by the runtime based on:
- Pending wake hints being converted to ticks
- External wake-only callback delivery
- Runtime scheduling decisions

The model cannot directly call `SystemTick`.

### 5. Add proactive ticks only when the policy is explicit

If Holon later adds a real proactive tick loop, it should do so with strict
rules:

- bounded retries
- backoff
- clear wake reasons
- auditability
- explicit mode or policy gates

Otherwise the agent will drift into uncontrolled self-looping.

**Implementation status:** Wake hints are coalesced via `pendingWakeHint` in agent state. The runtime only emits `SystemTick` when the agent is in an eligible state (`booting`, `awake_idle`, or `asleep`), preventing uncontrolled self-looping.

## A Clean Mental Model

The cleanest split is:

- `Sleep` = the model says "I am done for now"
- `InternalFollowup` = the model says "queue this next step"
- `TimerTick` = a scheduled externalized wakeup
- `TaskResult` / `TaskStatus` = delegated work rejoins the main session
- `SystemTick` = the runtime says "check again now"

This lets Holon support both:

- event-driven long-lived execution
- future proactive behavior

Without collapsing model intent and scheduler authority into one mechanism.

## Short Version

- Holon already has a solid event-driven liveness model with runtime-owned sleep.
- Current sleep is primarily runtime-driven, with optional model-requested
  early sleep.
- `SystemTick` is runtime-owned and NOT directly callable by the model. ✓ SHIPPED
- `InternalFollowup` is implemented for explicit same-agent continuation. ✓ SHIPPED
- `Sleep(duration_ms)` is a potential future addition (not yet implemented).
- Wake hints are implemented via `pendingWakeHint` coalescing. ✓ SHIPPED
- The right long-term direction is explicit separation between:
  - model intent
  - queued continuation
  - runtime scheduling
