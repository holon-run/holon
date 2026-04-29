---
title: RFC: Operator Notification and Intervention
date: 2026-04-23
status: draft
issue:
  - 375
---

# RFC: Operator Notification and Intervention

## Summary

Holon should expose a lightweight runtime primitive for notifying the relevant
operator.

The phase-1 model is intentionally small:

- agents use one explicit tool to notify the operator
- that tool accepts one required text field: `message`
- successful use creates an operator-facing notification record/event
- successful use does not stop the current turn
- successful use does not set `waiting_reason = awaiting_operator_input`
- whether to continue, sleep, or do other work remains the agent's decision

This replaces the heavier earlier direction where `RequestOperatorInput` would
create an agent-wide operator-gated wait.

## Why

Holon needs a reliable way for an agent to surface something to the operator
without relying on ordinary assistant prose being noticed.

However, making this primitive automatically stop the agent and gate future
model execution is too heavy for phase 1:

- it requires active wait records
- it requires scheduler gating
- it complicates non-operator ingress
- it makes child-agent supervision harder
- it forces the runtime to decide whether the agent is blocked

The simpler phase-1 contract should only answer:

- what should be shown to the operator?
- which operator boundary should receive it?
- how can delivery surfaces route it?

The agent can still choose to wait by calling `Sleep` after notifying the
operator, or it can continue working if useful.

## Scope

This RFC defines:

- the explicit runtime primitive for notifying the operator
- the minimum phase-1 tool input shape
- operator target resolution for default, public, and private child agents
- the operator-facing notification record/event
- relationship to remote operator transport delivery
- relationship to future operator-wait semantics

This RFC does not define:

- an agent-wide operator gate
- active operator-wait records
- approval buttons, forms, or structured response schemas
- provider-specific notification delivery
- a general human workflow engine

## Relationship To Existing RFCs

This RFC builds on, and does not replace:

- [Agent Profile Model](./agent-profile-model.md)
- [Remote Operator Transport and Delivery](./remote-operator-transport-and-delivery.md)
- [Result Closure](./result-closure.md)
- [Continuation Trigger](./continuation-trigger.md)
- [Waiting Plane And Reactivation](./waiting-plane-and-reactivation.md)

Remote operator transport can deliver operator notifications to an external
operator surface, but this RFC does not require Holon core to embed
provider-specific IM SDKs.

`awaiting_operator_input` remains a valid runtime waiting reason, but phase 1
does not create it through this notification primitive.

## Phase-1 Primitive

### Tool shape

Holon should expose one explicit tool for notifying the operator.

Suggested public tool name:

```text
NotifyOperator
```

Phase 1 should keep the input minimal:

```json
{
  "message": "I found two viable approaches. I will continue with option A unless you prefer option B."
}
```

Rules:

- `message` is required
- `message` is free-form text and may be multi-line
- phase 1 does not define required `kind`, `choices`, approval schema, or
  response schema

Optional future fields may include:

- `summary`
- `urgency`
- `expect_response`
- `related_work_item_id`

Those should not be required for phase 1.

### Tool availability

`NotifyOperator` should be a core agent tool.

It should be available to:

- the default agent
- public named agents
- private child agents

The tool is not a permission-expanding surface. It only records and emits an
operator-facing notification.

### Operator target

The operator target depends on the agent's supervision boundary:

- for the default agent, the operator is the primary Holon operator
- for a public named agent, the operator is the primary Holon operator
- for a private child agent, the operator is its parent/supervisor boundary

A private child should not acquire its own remote operator transport route just
because it notified the operator. Its notification should surface through the
parent/supervision boundary, and any external operator delivery remains owned by
the parent/default operator surface.

## Turn Semantics

### Non-terminal by default

Successful `NotifyOperator` is not a terminal tool round.

That means:

- the tool succeeds
- the current execution pass may continue
- the runtime does not settle into `waiting`
- the waiting reason does not become `awaiting_operator_input`
- ordinary scheduling is not gated

If the agent wants to stop after notifying the operator, it should explicitly
call `Sleep` when it is safe to rest.

If the agent wants to continue with a reasonable default, it can do so after
notifying the operator.

### Runtime evidence, not wait evidence

Using this tool creates explicit runtime evidence that the operator was
notified.

It does not create explicit runtime evidence that the agent is blocked.

Holon should not infer an operator wait from prose such as:

- "please tell me what to do next"
- "I need your approval"
- "I will wait for you"

If Holon later adds a heavier wait primitive, that primitive should have its own
contract rather than being inferred from `NotifyOperator`.

## Notification Record And Event

Successful use should create an operator-facing notification record/event.

Suggested event:

```ts
OperatorNotificationRequested {
  notification_id: string
  agent_id: string
  requested_by_agent_id: string
  target_operator_boundary: 'primary_operator' | 'parent_supervisor'
  message: string
  summary?: string
  work_item_id?: string
  correlation_id?: string
  causation_id?: string
  created_at: string
}
```

The summary may be derived from `message`:

- use the first non-empty line
- trim surrounding whitespace
- truncate to a bounded operator-facing length when needed

Operator-facing surfaces should be able to show:

- which agent sent the notification
- whether it came from a private child
- the derived short summary
- the full message
- delivery status when external delivery is configured

## Delivery

`NotifyOperator` produces an operator-facing event. It does not itself know how
to send Telegram, Slack, Matrix, email, or any other provider message.

If a delivery router is configured, the event may be delivered using the
operator transport route resolution described by
[Remote Operator Transport and Delivery](./remote-operator-transport-and-delivery.md).

If no delivery route is configured, the notification remains inspectable through
status, transcript, event stream, or TUI surfaces.

Remote operator transport delivery should treat this event like other
operator-facing output:

- resolve an inbound reply route when available
- otherwise resolve a waiting/work-item or agent default operator route
- submit a delivery intent through the binding's `delivery_callback_url`
- treat 2xx as `accepted_by_transport`

Provider-level delivery status remains transport-owned.

## Relationship To Operator Replies

A later operator message is still ordinary operator input.

It may:

- enter as `operator_prompt`
- override or redirect current work according to normal operator instruction
  semantics
- be correlated with the notification when metadata makes that possible

But there is no active operator wait to satisfy in phase 1. The runtime should
not require a reply, and it should not automatically clear a wait record.

## Relationship To `awaiting_operator_input`

This RFC intentionally does not use `awaiting_operator_input` in phase 1.

That waiting reason should be reserved for a future explicit wait primitive if
Holon decides it needs one.

The future primitive might be named something like:

- `RequestOperatorInput`
- `AwaitOperatorInput`
- `RequestOperatorDecision`

That future design can decide:

- whether the wait is agent-wide or work-item-bound
- whether ordinary scheduling is gated
- whether duplicate waits are rejected
- how operator replies target a specific wait

Those questions should not block the lightweight notification primitive.

## Relationship To Other Ingress

Since `NotifyOperator` does not gate the agent:

- callback deliveries continue to behave normally
- task results continue to behave normally
- timer events continue to behave normally
- external trigger deliveries continue to behave normally
- administrative control events continue to behave normally

No special ingress rejection or coalescing rule is introduced by this RFC.

## Non-Goals

Phase 1 should not attempt to define:

- provider-specific notification routing
- inbox or IM provider adapters
- approval buttons or structured form responses
- an operator-gated scheduler state
- active operator-wait lifecycle
- a general-purpose human workflow DSL

## Initial Direction

The intended phase-1 direction is:

1. add one explicit operator notification tool, `NotifyOperator`
2. keep the tool input to a single required `message` field
3. derive operator-facing summary text from the message
4. make successful use non-terminal by default
5. do not set `waiting_reason = awaiting_operator_input`
6. do not gate ordinary model scheduling
7. resolve private child notifications through the parent/supervision boundary
8. emit an operator-facing notification record/event
9. let the delivery router optionally deliver the event through remote operator
   transport
10. defer heavier operator-wait semantics to a future RFC

## Deferred Design: Explicit Operator Wait

A future RFC may still introduce a heavier operator-wait primitive.

That design should be separate from `NotifyOperator` and should explicitly
settle:

- whether the wait is agent-wide or work-item-bound
- whether the current turn must stop
- whether ordinary model scheduling is gated
- whether other ingress is accepted but not model-visible
- how operator replies target and satisfy the wait
- what runtime state tracks active waits

Until then, `NotifyOperator` should remain a notification primitive, not a
hidden waiting primitive.
