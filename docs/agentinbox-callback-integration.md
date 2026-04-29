# AgentInbox Callback Integration

Canonical RFC: `docs/rfcs/external-trigger-capability.md`.

This note documents the earlier callback-ingress shape. New public vocabulary
should describe this as **External Trigger Capability** rather than as a generic
callback channel.

This note captures the first callback-ingress contract for integrating `AgentInbox`
with `Holon`.

## Problem

`AgentInbox` already defines source-specific notification payloads.

`Holon` should not force those notifications through a second business-level schema
such as:

- `text`
- `json`
- `reason`

That shape makes `Holon` act like an application webhook hub instead of a runtime.

## Decision

`Holon` callback ingress should be:

- route-aware
- payload-light
- source-agnostic

More concretely:

1. Callback URLs encode delivery mode in the path:
   - `/callbacks/enqueue/:token`
   - `/callbacks/wake/:token`
2. The descriptor remains the source of truth for delivery mode.
3. The request body is treated as opaque callback content:
   - `application/json` becomes `MessageBody::Json`
   - `text/*` becomes `MessageBody::Text`
   - other content types are wrapped into a JSON envelope with `content_type` and `body_base64`

## Why Encode Mode In The URL

This gives the agent a cleaner capability to hand to external systems.

It avoids making external systems choose delivery mode via request-body fields, and it
matches the actual semantic choice the agent is making:

- wake me because something changed
- enqueue this payload because it is meaningful input

## Enqueue Message

For `enqueue_message`:

- the callback body becomes the `CallbackEvent` body
- `Holon` does not reinterpret source-specific fields
- the event metadata carries runtime lineage and callback provenance

This means `AgentInbox` can keep its own notification schema and `Holon` simply turns
it into a trusted agent-visible event.

## Wake Only

`wake_only` should not be a blind trigger.

If a callback wakes an agent, the agent still needs to know:

- what source woke it
- what resource or interest it should inspect
- what payload caused the activation

So `wake_only` does not enqueue a normal `CallbackEvent`, but it does preserve an
activation context:

- source
- resource
- reason
- content type
- callback body

The runtime exposes that activation context to the agent in prompt assembly during the
turn triggered by the wake.

## Scope Boundary

This contract changes callback ingress only.

It does not change public `/enqueue`, which remains a more structured runtime-facing
API.
