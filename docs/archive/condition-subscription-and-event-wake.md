# Condition Subscription And Event Wake

This note captures a likely future direction for how Holon should model
"call me back when something happens" semantics.

The motivating problem is simple:

- an agent opens a pull request
- or asks for review
- or triggers CI
- then needs to wait for a condition to become true

Today the most obvious implementation is polling:

- sleep for a while
- check again later

That works, but it is not the right long-term abstraction.

## The Key Framing

The right primitive is usually not a callback.

It is:

- a condition subscription
- or a wake registration

The distinction matters.

A raw callback model would mean the agent somehow registers executable behavior
to run later. That creates immediate problems:

- hard to audit
- hard to persist
- hard to recover after restart
- hard to secure
- unclear for external systems to execute

A condition subscription model is simpler:

- the agent declares what condition it cares about
- the runtime records that declaration
- when the condition becomes true, the runtime enqueues a normal wake message

So the agent does not register code.

It registers intent.

## Proposed Primitive

Holon should eventually support a first-class object like:

- `WatchRecord`
- or `SubscriptionRecord`

The runtime model could look roughly like this:

```text
WatchRecord {
  id
  target_agent_id
  source
  resource
  condition
  on_trigger
  trigger_turn
  expires_at
  dedupe_key
  correlation_id
  causation_id
}
```

## Meaning Of The Fields

### `target_agent_id`

Which agent should receive the wakeup when the condition becomes true.

### `source`

Which integration or runtime domain owns the watch.

Examples:

- `github`
- `gitlab`
- `slack`
- `email`
- `internal`

### `resource`

The thing being watched.

Examples:

- `pull_request:123`
- `ci_run:abc`
- `issue:99`

### `condition`

A declarative condition, not executable code.

Examples:

- `required_checks_passed`
- `review_submitted`
- `review_state=approved`
- `status=failed`

### `on_trigger`

The message template or payload to enqueue when the condition fires.

This should remain data, not code.

### `trigger_turn`

Whether the target agent should merely receive queued mail or should also be
woken immediately when the condition fires.

### `expires_at`

So subscriptions do not live forever by accident.

### `dedupe_key`

Prevents duplicate wakeups for the same condition.

### `correlation_id` / `causation_id`

Preserves lineage back to the original request.

## Why This Is Better Than Polling

Polling is really:

- agent-owned waiting

Condition subscription is:

- runtime-owned waiting

That is the core difference.

With polling:

- the agent must remember to check again
- the agent chooses an interval
- the system spends turns just rediscovering that nothing changed

With subscriptions:

- the agent states the condition once
- the runtime or integration waits on its behalf
- the agent wakes only when something relevant happened

This is a better fit for long-lived agents.

## Push And Poll Should Be Hidden Behind The Same Abstraction

One important design choice:

The agent should not need to know whether the condition is implemented by:

- push
- or polling

The runtime can satisfy a watch in two ways.

### Push-backed watch

Examples:

- GitHub webhook
- CI webhook
- review event
- IM reply

These are ideal.

When the event arrives, the runtime matches it against active watches and
enqueues a wake message.

### Poll-backed watch

If the source system has no push API, the runtime can poll in the background.

But this should remain an implementation detail of the adapter or integration
layer, not something the agent has to manually orchestrate every time.

So the agent always expresses:

- "wait for this condition"

Not:

- "please poll every 300 seconds"

## Example: Pull Request Workflow

Suppose an agent opens a PR.

Instead of saying:

- sleep 5 minutes
- check CI
- sleep 5 minutes
- check review

It should be able to say something closer to:

- wait for PR `#123` required checks to pass
- wake me when review is submitted
- wake me if CI fails

Then the runtime stores watches and later delivers a normal queued message when
one fires.

For example:

```text
origin = integration
kind = webhook_event   // or future watch_trigger
metadata.watch_id = ...
metadata.source = github
metadata.resource = pull_request:123
causation_id = original_wait_request
body = {
  pr: 123,
  checks: "passed",
  review_state: "approved"
}
```

The agent then continues through the normal loop and decides what to do:

- merge
- respond to reviewer
- fix CI
- escalate to another agent

## Why This Should Still Go Through The Main Queue

Even when a watch fires, it should not bypass the runtime.

The trigger should still become a normal queued message.

That preserves:

- origin
- trust
- wake reason
- auditability
- consistent scheduling

So a watch firing should not directly execute code or tools.

It should enqueue a message and let the agent loop handle it.

## Relationship To Other Trigger Primitives

This fits naturally next to the other runtime triggers already discussed.

- `Sleep(duration_ms)` = time-based wake
- `InternalFollowup` = explicit continuation
- `SystemTick` = runtime-owned reconsideration
- `ConditionSubscription` = external-condition-based wake

These are different primitives and should remain distinct.

## Recommendation

If Holon grows in this direction, it should avoid exposing callback-style
semantics and instead provide a declarative wait primitive.

The cleanest rule is:

- agents express conditions
- runtime owns waiting
- integrations own delivery details
- wakeups always re-enter through the queue

## Short Version

- "Call me back when CI passes" should not be modeled as a raw callback.
- It should be modeled as a declarative condition subscription.
- Push and polling should be hidden behind the same runtime abstraction.
- When the condition fires, Holon should enqueue a normal wake message for the
  target agent.
