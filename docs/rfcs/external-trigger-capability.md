---
title: RFC: External Trigger Capability And Providerless Ingress
date: 2026-04-23
status: draft
issue:
  - 371
---

# RFC: External Trigger Capability And Providerless Ingress

## Summary

Holon should model callback-backed providerless ingress as an **External
Trigger Capability**.

The public waiting-plane tools should be:

- `CreateExternalTrigger`
- `CancelExternalTrigger`

The implementation should use `ExternalTriggerCapability` and
`ExternalTriggerRecord` for the runtime model. The HTTP endpoint may keep
callback-oriented transport names where they describe the delivery mechanism.

An external trigger lets an agent say:

- I am waiting on an external condition
- here is a scoped capability an external system may use when that condition
  changes
- when that capability is used, Holon should either wake me or enqueue the
  delivered content

This keeps Holon providerless. Holon does not need to understand GitHub,
AgentInbox, Slack, CI, email, or any provider-specific subscription schema.

## Why

The earlier callback terminology is accurate at the HTTP implementation layer,
but it is not the best public runtime abstraction.

`callback` makes the feature sound like:

- a generic webhook hub
- an HTTP implementation detail
- a provider-specific integration endpoint

The runtime meaning is narrower and more useful:

- an agent creates a managed waiting intent
- Holon returns a scoped external trigger capability
- an external system may use that capability later
- Holon validates the capability and reactivates the target agent according to
  an explicit delivery mode

`ExternalTrigger` is also easier to distinguish from:

- remote operator transport
- public enqueue
- callback internals
- operator wait

## Core Vocabulary

### External trigger

An external trigger is a waiting-plane object that allows an external system to
reactivate a specific agent later.

It is not:

- an operator message
- a generic public enqueue endpoint
- a provider subscription schema
- a permission to execute code
- a model-visible provider SDK

### External trigger capability

The capability is the token-bearing object returned to the agent. The agent may
hand it to an external tool, skill, MCP server, worker, or integration service.

The capability should be treated as bearer authority for one scoped ingress
surface. Possession of the capability is enough to deliver to that specific
external trigger, subject to descriptor status and delivery-mode checks.

### Waiting intent

The waiting intent records what the agent is waiting for:

- summary
- source
- condition
- optional resource
- delivery mode
- target agent
- optional work-item anchor

The external trigger is the ingress capability attached to that waiting intent.

### Providerless ingress

Providerless ingress means Holon validates and normalizes the capability use,
but does not interpret provider-specific payloads.

Holon should not know whether the payload came from GitHub, AgentInbox, email,
CI, Slack, or another external system.

## Tool Surface

### `CreateExternalTrigger`

Phase 1 should expose:

```json
{
  "summary": "wait for CI callback",
  "source": "github",
  "condition": "required_checks_passed",
  "resource": "pull_request:123",
  "delivery_mode": "wake_only"
}
```

Fields:

- `summary`: required short human/model-readable summary
- `source`: required provider or integration source label
- `condition`: required condition label
- `resource`: optional provider-specific resource identifier
- `delivery_mode`: `wake_only` or `enqueue_message`

The tool creates:

- one waiting intent
- one external trigger descriptor
- one scoped trigger URL

The tool returns an external trigger capability:

```ts
ExternalTriggerCapability {
  waiting_intent_id: string
  external_trigger_id: string
  trigger_url: string
  target_agent_id: string
  delivery_mode: 'wake_only' | 'enqueue_message'
}
```

### `CancelExternalTrigger`

Phase 1 should expose:

```json
{
  "waiting_intent_id": "..."
}
```

Cancellation should:

- mark the waiting intent as cancelled
- revoke the attached external trigger descriptor
- make future use of the trigger URL fail
- preserve audit records

## Delivery Modes

External triggers have an explicit delivery mode. The external caller should
not choose delivery semantics by request body.

### `enqueue_message`

`enqueue_message` means the delivered payload is meaningful input.

On valid delivery:

- Holon validates the trigger token
- Holon checks the descriptor and waiting intent are still active
- Holon preserves the request body as opaque content
- Holon enqueues an integration event for the target agent

The payload is opaque to Holon:

- JSON bodies become JSON message bodies
- text bodies become text message bodies
- other content types may be wrapped with content type and base64 body

Holon should not reinterpret provider-specific fields.

### `wake_only`

`wake_only` means something changed and the agent should reconsider external
state, but the delivery should not become a normal queued external-trigger
event.

On valid delivery:

- Holon validates the trigger token
- Holon checks the descriptor and waiting intent are still active
- Holon records a wake hint
- Holon preserves activation context for prompt/status/audit surfaces

Activation context may include:

- source
- resource
- reason
- content type
- callback body or opaque body envelope
- correlation and causation ids

`wake_only` is not a blind ping. The agent should be able to understand what
changed well enough to inspect the relevant source through tools.

## Ingress Contract

The trigger URL is an opaque capability URL from the external system's
perspective.

Holon may encode delivery mode in the URL path and should still verify that the
URL path mode matches the descriptor's stored delivery mode. A mode mismatch
must be rejected.

Every delivery must resolve to:

- one active external trigger descriptor
- one active waiting intent
- one target agent

There is no broadcast-by-default behavior.

If the target agent is administratively stopped, delivery should fail without
side effects and should tell the caller that the agent must be resumed first.

## Provenance And Authority Labels

External trigger deliveries should use the provenance and authority model from
[Provenance, Admission, and Authority](./default-trust-auth-and-control.md).

For `enqueue_message`, the message projects to:

```ts
origin: { kind: 'callback', descriptor_id: '...', source: '...' }
delivery_surface: 'http_callback_enqueue'
admission_context: 'external_trigger_capability'
authority_class: 'integration_signal'
```

For `wake_only`, the wake hint or resulting runtime-owned system tick should
preserve equivalent trigger provenance in metadata:

```ts
delivery_surface: 'http_callback_wake'
admission_context: 'external_trigger_capability'
authority_class: 'integration_signal'
```

A valid external trigger proves that Holon accepted the capability use. It does
not make the payload an operator instruction. The payload may trigger
continuation or satisfy an external waiting intent, but it should not override
operator instructions.

## Relationship To Operator Notification And Wait

External triggers are separate from operator notifications.

`NotifyOperator` creates an operator-facing notification. It does not create an
operator-gated wait, so there is no wait for an external trigger to satisfy or
clear.

If Holon later adds an explicit operator-wait primitive, external trigger
deliveries should still not satisfy that wait by default. Only operator input
should satisfy an operator wait unless a future RFC says otherwise.

## Relationship To Remote Operator Transport

Remote operator transport and external triggers are different surfaces.

Remote operator transport is for:

- the operator sending direct input
- the runtime delivering user-facing output back to the operator

External triggers are for:

- external systems
- providerless reactivation
- AgentInbox or provider adapters delivering integration signals
- CI, GitHub, email, message bus, and similar machine/integration events

Both may involve AgentInbox or an HTTP hop, but they must not share authority
semantics.

## Relationship To Waiting Plane

External triggers are part of the waiting plane.

They are the public sub-family for external conditions:

- `CreateExternalTrigger`
- `CancelExternalTrigger`

They should remain distinct from:

- `Sleep`, which is local rest/idle posture
- `NotifyOperator`, which emits an operator-facing notification
- task waiting, which waits for delegated task results
- remote operator transport, which carries operator input and output

## Persistence And Cleanup

Active waiting intents and external trigger descriptors must survive restart.

Cancellation must revoke the trigger descriptor and keep an audit trail.

Repeated deliveries may be accepted while the waiting intent remains active.
The agent or runtime should cancel obsolete triggers when:

- the relevant external condition is no longer needed
- the active work item changes
- the waiting intent becomes stale
- the user or runtime explicitly cancels it

Time-based expiry remains a future enhancement unless implemented separately.

## Naming Boundary

The runtime and public tool contract use external-trigger vocabulary:

- `CreateExternalTrigger`
- `CancelExternalTrigger`
- `ExternalTriggerCapability`
- `ExternalTriggerRecord`
- `external_trigger_id`
- `trigger_url`

Callback-oriented names may remain only where they describe the HTTP transport:

- `/callbacks/enqueue/:token`
- `/callbacks/wake/:token`
- `MessageKind::CallbackEvent`

Implementation-internal callback names may remain where they describe the HTTP
callback mechanism. Public model-facing and RFC vocabulary should use external
trigger.

## Non-Goals

This RFC does not define:

- provider-specific integration adapters
- provider-specific subscription schemas
- a generic webhook business-logic hub
- operator input or operator delivery
- approval buttons or human workflow forms
- a full enterprise authorization matrix

## Initial Direction

The phase-1 direction is:

1. rename the public concept to External Trigger Capability
2. add `CreateExternalTrigger` and `CancelExternalTrigger`
3. preserve the existing `wake_only` and `enqueue_message` delivery modes
4. project deliveries as `integration_signal`
5. keep callback payloads opaque to Holon core
6. preserve current token validation, mode mismatch rejection, stopped-agent
   rejection, cancellation, and restart behavior
8. align prompt guidance, docs, tests, and event surfaces with the new
   vocabulary
