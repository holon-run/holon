# Wake Hint Vs Contentful Event

This note captures a useful distinction for Holon's external ingress model:

- some external input carries meaningful content that should enter the agent's
  queue and eventually the model context
- some external input is only a wake signal

Those should not be treated as the same kind of thing.

## The Problem

Today the simplest ingress model is:

- an external event arrives
- Holon normalizes it into a message
- the message enters the queue
- the agent eventually processes it

That is correct for real messages.

But not every external trigger is really a message.

Sometimes an outside system only wants to say:

- wake up
- something changed
- check again

In these cases, forcing everything through the same contentful message path
creates problems:

- unnecessary queue noise
- transcript pollution
- prompt pollution
- too many model-visible "empty" events

## Two Different Ingress Semantics

Holon should eventually distinguish between:

### 1. Contentful event

A contentful event is real input.

Examples:

- a webhook payload
- an IM message
- a task result
- a review summary
- a CI failure payload

Properties:

- should enter the queue
- should preserve origin and trust
- may become model-visible context
- should be auditable as an actual message

### 2. Wake hint

A wake hint is not really a message.

It is a signal that the runtime may want to reconsider the session.

Examples:

- "something about this PR changed"
- "mailbox changed"
- "you may want to check pending conditions"
- "an outside watcher thinks work may now be possible"

Properties:

- does not need to enter model context directly
- should not become transcript noise by default
- should not force a turn when the runtime is already busy

## Why This Should Not Be Just Another Message Kind

It is tempting to model a wake hint as a normal message and queue it like any
other event.

That is usually the wrong shape.

A wake hint is better understood as:

- a control-plane signal

Not:

- a data-plane input message

If Holon models wake hints as ordinary contentful events, it will encourage the
runtime to treat every "poke" as if it were meaningful model input.

That is not what we want.

## Proposed Behavior

The better behavior is:

1. an outside system sends a wake hint
2. Holon inspects the current agent state
3. Holon decides whether the hint should be ignored, coalesced, or converted
   into an internal wake
4. only then does the runtime decide whether to schedule a real turn

So the wake hint does not directly become a queued prompt item.

It becomes a runtime decision point.

## Relationship To `SystemTick`

This fits cleanly with the earlier trigger discussion.

- external systems should not directly send `SystemTick`
- `SystemTick` remains runtime-owned
- a wake hint may cause the runtime to emit a `SystemTick`

That preserves the intended separation:

- external actor says: "consider waking"
- runtime decides: "yes, start another turn now"

So the wake hint is best thought of as:

- permission for the runtime to consider a `SystemTick`

Not:

- a turn request by itself

## Suggested Runtime Rule

Holon should not decide based only on "sleeping or not sleeping."

A better rule is:

- if the agent is idle and runtime policy allows reconsideration, a wake hint
  may trigger a runtime-owned wake
- if the agent is already actively running, the wake hint should usually be
  dropped or coalesced
- if the agent is paused or otherwise not eligible for wake, the hint should be
  ignored or logged only

So the decision boundary is:

- should the runtime start a new turn now?

Not merely:

- is the state exactly `Asleep`?

## Coalescing Matters

Wake hints should be coalescible.

If ten outside systems send:

- "something changed"

Holon should not start ten future turns or queue ten nearly identical wake
signals.

A better model is:

- store a wake-needed flag or a revision marker
- collapse repeated hints
- let one wake decision cover the current burst

This is important for:

- mailbox traffic
- noisy CI systems
- repeated external polling relays
- bursty condition-watch triggers

## Wake Hints Vs Condition Subscriptions

This distinction also fits well with condition subscriptions.

When a condition subscription fires, two different delivery modes might exist:

### Mode 1: contentful trigger

The external system knows enough to provide a structured payload.

In that case, Holon should enqueue a normal message with the condition result.

### Mode 2: wake-only trigger

The external system only knows that something relevant changed.

In that case, Holon should receive a wake hint and let the agent decide what to
check next once woken.

This lets integrations stay lightweight without polluting the prompt stream.

## Example

Suppose an external watcher notices that a PR's state changed, but does not
want to send the entire updated PR object.

It can send:

- wake target agent `A`
- correlation id `X`
- reason: `pr_state_changed`

Holon then decides:

- if `A` is idle, emit one runtime-owned continuation wake
- if `A` is already running, drop or coalesce the hint

When the agent wakes, it can decide to:

- query GitHub through whatever tool it already has
- inspect the current PR state
- continue work

## Recommendation

Holon should eventually distinguish between:

- ingress that is meaningful content
- ingress that is only a wake hint

And it should do so at the runtime level, not just as a prompt convention.

The cleanest rule is:

- contentful events go into the queue as messages
- wake hints do not directly enter the queue
- wake hints are handled by runtime wake policy
- if needed, the runtime converts them into an internal `SystemTick`

## Short Version

- Not every external trigger should become a model-visible message.
- Holon should support a pure wake hint separate from contentful events.
- A wake hint should usually be ignored if the agent is already busy.
- If the runtime decides to act on it, it should convert it into a
  runtime-owned wake such as `SystemTick`, not expose it directly as user-style
  input.
