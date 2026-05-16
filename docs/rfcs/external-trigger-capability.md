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

An external trigger capability is an agent-level ingress capability. It is not a
WorkItem-owned waiting resource, and it is not partitioned by provider/source.
Capability identity is the target agent plus the delivery mode.

The runtime should provision and expose a default external ingress capability
for each agent and delivery mode. Agents should not normally create or cancel
callback ingress as part of a wait. Waiting state records what the agent is
waiting for; the ingress capability is long-lived agent infrastructure.

Compatibility tool names such as `CreateExternalTrigger` and
`CancelExternalTrigger` may remain for migration, diagnostics, or explicit
administrative actions, but they should not be the ordinary wait workflow.
Longer-term surfaces should expose ingress descriptors from agent context,
status, or provider configuration instead of requiring model-facing creation.

The implementation should use `ExternalTriggerCapability` and
`ExternalTriggerRecord` for the runtime model. The HTTP endpoint may keep
callback-oriented transport names where they describe the delivery mechanism.

An external trigger lets an agent say:

- this agent has a stable external ingress endpoint
- an external system may use that endpoint later
- when that endpoint is used, Holon should either wake the agent or enqueue the
  delivered content according to `delivery_mode`

This keeps Holon providerless. Holon does not need to understand GitHub,
AgentInbox, Slack, CI, email, or any provider-specific subscription schema.
Provider/source labels belong to delivery provenance, provider subscription
metadata, or waiting-state metadata, not to capability identity.

## Why

The earlier callback terminology is accurate at the HTTP implementation layer,
but it is not the best public runtime abstraction.

`callback` makes the feature sound like:

- a generic webhook hub
- an HTTP implementation detail
- a provider-specific integration endpoint

The runtime meaning is narrower and more useful:

- the runtime provisions an agent-level ingress capability
- Holon exposes or returns the existing capability for that agent and
  delivery mode
- an external system may use that capability later
- Holon validates the capability and reactivates the target agent according to
  an explicit delivery mode

`ExternalTrigger` is also easier to distinguish from:

- remote operator transport
- public enqueue
- callback internals
- operator wait
- WorkItem waiting state

## Core Vocabulary

### External trigger

An external trigger is an agent-level ingress object that allows an external
system to reactivate a specific agent later.

It is not:

- an operator message
- a generic public enqueue endpoint
- a provider subscription schema
- a permission to execute code
- a model-visible provider SDK

### External trigger capability

The capability is the token-bearing object returned to the agent. The agent may
hand it to an external tool, skill, MCP server, worker, or integration service.

The capability should be treated as bearer authority for one agent ingress
surface and one delivery mode. Possession of the capability is enough to deliver
to that specific external trigger, subject to descriptor status and
delivery-mode checks.

### Waiting intent

The waiting intent records what the agent is waiting for:

- description
- source
- external reference metadata
- target agent
- optional work-item anchor
- optional external trigger id to use for callbacks

Waiting intents may reference an external trigger capability, but they do not
own it. Cancelling or completing a wait must not revoke the agent-level ingress
capability.

### Providerless ingress

Providerless ingress means Holon validates and normalizes the capability use,
but does not interpret provider-specific payloads.

Holon should not know whether the payload came from GitHub, AgentInbox, email,
CI, Slack, or another external system.

## Default Ingress Surface

The default public contract is:

- the runtime provisions one active external trigger descriptor per target agent
  and `delivery_mode`
- the agent context, status, or provider configuration may expose the descriptor
  needed by trusted integration code
- provider adapters, skills, MCP servers, or external systems register their
  watches using that descriptor
- WorkItem `blocked_by` or future external-wait records describe why the agent
  is waiting and what to inspect after waking
- ordinary wait creation, completion, or cleanup does not mint or revoke
  callback ingress

### Compatibility/Admin Surfaces

`CreateExternalTrigger` may remain as a compatibility or diagnostic name for
returning the default agent ingress capability:

```json
{
  "delivery_mode": "wake_hint"
}
```

Fields:

- `delivery_mode`: `wake_hint` or `enqueue_message`

`source`, `description`, and `scope` are intentionally not part of the external
trigger capability creation contract. Source is delivery provenance, provider
subscription metadata, or waiting-intent metadata. Description belongs to the
wait or WorkItem state that explains what the agent should inspect after waking.

The tool is idempotent for the current agent and `delivery_mode`: if an active
capability already exists, the runtime should return it instead of minting a new
URL. One agent may have one default ingress per delivery mode. It is not the
ordinary workflow for waiting on external conditions.

The runtime creates, if needed:

- one external trigger descriptor
- one scoped trigger URL

The tool returns an external trigger capability:

```ts
ExternalTriggerCapability {
  external_trigger_id: string
  trigger_url: string
  target_agent_id: string
  delivery_mode: 'wake_hint' | 'enqueue_message'
  status: 'active'
}
```

Waiting state is represented separately. Today an agent can explain the wait in
WorkItem `blocked_by`. A future `RecordExternalWait` tool may record
`description`, `source`, `external_ref`, optional `work_item_id`, and optional
`external_trigger_id`. A corresponding wait-cancellation tool would cancel the
waiting state only; it would not revoke the ingress capability.

### `CancelExternalTrigger`

Explicit capability revocation should be an admin/diagnostic operation:

```json
{
  "external_trigger_id": "..."
}
```

Cancellation is a low-frequency capability revocation operation. It should:

- revoke the agent-level external trigger descriptor
- make future use of the trigger URL fail
- preserve audit records

Normal WorkItem completion or waiting-intent cleanup should not call
`CancelExternalTrigger` unless the agent intentionally wants to revoke or rotate
the ingress capability. If this operation remains model-facing, a clearer
long-term name is `RevokeExternalIngress` or `RotateExternalIngress`.

## Delivery Modes

External triggers have an explicit delivery mode. The external caller should
not choose delivery semantics by request body.

### `enqueue_message`

`enqueue_message` means the delivered payload is meaningful input.

On valid delivery:

- Holon validates the trigger token
- Holon checks the descriptor is still active and targets the agent
- Holon preserves the request body as opaque content
- Holon enqueues an integration event for the target agent

The payload is opaque to Holon:

- JSON bodies become JSON message bodies
- text bodies become text message bodies
- other content types may be wrapped with content type and base64 body

Holon should not reinterpret provider-specific fields.

### `wake_hint`

`wake_hint` means something changed and the agent should reconsider external
state, but the delivery should not become a normal queued external-trigger
message.

On valid delivery:

- Holon validates the trigger token
- Holon checks the descriptor is still active and targets the agent
- Holon records or updates a pending wake hint
- Holon preserves activation context for prompt/status/audit surfaces

Activation context should include:

- external trigger id
- target agent id
- delivery mode
- optional or correlated source
- content type
- callback body or opaque body envelope
- correlation and causation ids
- correlated waiting intent ids, if any

`wake_hint` is not a blind ping. The agent should be able to understand which
trigger fired and which source to inspect from provenance or correlated wait
state. The delivered payload
is still opaque to Holon core and may be used as a hint, but `wake_hint` is
level-triggered rather than queue-triggered: if the agent is already busy or has
queued work, repeated wake hints may be coalesced into the latest pending hint.

This makes `wake_hint` appropriate for integrations that already have their own
durable queue or query surface. For example, AgentInbox should use an
agent-level `wake_hint` trigger. WorkItem `blocked_by` or future waiting
state can say to read unread inbox items. The webhook payload may include
counts, latest entry ids, and previews, but the agent should call `agentinbox inbox read` after waking instead of
treating the webhook payload as the only durable source of truth.

## Ingress Contract

The trigger URL is an opaque capability URL from the external system's
perspective.

Holon may encode delivery mode in the URL path and should still verify that the
URL path mode matches the descriptor's stored delivery mode. A mode mismatch
must be rejected.

Every delivery must resolve to:

- one active external trigger descriptor
- one target agent

Delivery may also correlate to zero or more active waiting intents, but it does
not require one.

There is no broadcast-by-default behavior, and there is no source-partitioned
routing by default.

If the target agent is administratively stopped, delivery should fail without
side effects and should tell the caller that the agent must be resumed first.

## Provenance And Authority Labels

External trigger deliveries should use the provenance and authority model from
[Provenance, Admission, and Authority](./default-trust-auth-and-control.md).

For `enqueue_message`, the message projects to:

```ts
origin: { kind: 'callback', descriptor_id: '...', source?: '...' }
delivery_surface: 'http_callback_enqueue'
admission_context: 'external_trigger_capability'
authority_class: 'integration_signal'
```

For `wake_hint`, the wake hint or resulting runtime-owned system tick should
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

External trigger capabilities support the waiting plane, but they are not
themselves waiting intents or WorkItem waiting resources.

Waiting state should be represented separately by WorkItem `blocked_by` today
or by a future external-wait record. That state may reference an
`external_trigger_id` but must not own or revoke it.

External trigger capabilities should remain distinct from:

- `Sleep`, which is local rest/idle posture
- `NotifyOperator`, which emits an operator-facing notification
- task waiting, which waits for delegated task results
- remote operator transport, which carries operator input and output

## Persistence And Cleanup

Active external trigger descriptors and any separate waiting-state records must
survive restart.

Capability revocation must revoke the trigger descriptor and keep an audit
trail. Waiting-state cancellation must not revoke the trigger descriptor.

Repeated deliveries may be accepted while the external trigger descriptor
remains active. Cleanup depends on the resource being cleaned up:

- WorkItem cleanup may cancel or mark inactive WorkItem-bound waiting state.
- External-wait cleanup may cancel the wait record or remove its correlation to
  an external trigger id.
- Capability cleanup revokes only when the agent explicitly cancels or rotates
  the ingress capability, configured expiry fires, administrative cleanup runs,
  or the agent is removed.

One agent may keep one active default ingress per `delivery_mode`. It should not
be cancelled by WorkItem cleanup or by the absence of an active work item.

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
2. provision default agent-level external ingress for each supported
   `delivery_mode`
3. use `wake_hint` and `enqueue_message` as the model-facing delivery modes
4. project deliveries as `integration_signal`
5. keep callback payloads opaque to Holon core
6. preserve current token validation, mode mismatch rejection, stopped-agent
   rejection, cancellation, and restart behavior
7. remove user-visible `work_item`/`agent` trigger scopes and make capability
   identity agent-level, partitioned only by `delivery_mode`
8. keep `CreateExternalTrigger`/`CancelExternalTrigger` only as compatibility,
   diagnostic, or admin surfaces; do not require them for ordinary waits
9. align prompt guidance, docs, tests, and event surfaces with the new
   vocabulary
