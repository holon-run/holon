---
title: RFC: Remote Operator Transport and Delivery
date: 2026-04-23
status: draft
issue:
  - 380
---

# RFC: Remote Operator Transport and Delivery

## Summary

Holon should model a remote operator transport as an authenticated remote
surface for the primary operator, not as a high-authority external channel.

The phase-1 direction is:

- an operator-bound remote transport admits messages as `operator_prompt`
- the admitted message has `authority_class = operator_instruction`
- the admitted message still preserves transport provenance
- routing to an agent is resolved from an explicit binding, not from an
  implicit "current agent"
- AgentInbox-backed remote operator input should enter through a dedicated
  operator ingress endpoint, not the public enqueue endpoint
- replies are delivered by a runtime delivery router from user-facing output
  events, not by model-visible provider-specific send tools
- delivery targets resolve from the inbound reply route first, then waiting or
  work-item delivery policy, then the agent's default operator route

This RFC does not define Telegram, Slack, Matrix, or any other provider
adapter. It defines the runtime contract those adapters should satisfy.

## Why

Holon already distinguishes:

- operator input
- callback and webhook ingress
- channel input
- tool-fetched external evidence

The unresolved boundary is how to treat an IM or remote surface that is
actually controlled by the primary operator.

Treating all IM messages as `channel_event` is too coarse:

- a bound private Telegram DM from the owner is semantically operator input
- a group message from another participant is not operator input
- a machine callback from AgentInbox or GitHub is integration ingress
- public channel content is usually evidence, not authority

At the same time, treating a Telegram message as ordinary local operator input
would lose important audit information:

- which transport admitted it
- which binding authenticated it
- which provider conversation it came from
- where a user-facing reply should be delivered

Holon needs a model that preserves both facts:

- the input is from the operator
- the operator used a remote transport

## Relationship To Existing RFCs

This RFC builds on:

- [Provenance, Admission, and Authority](./default-trust-auth-and-control.md)
- [Continuation Trigger](./continuation-trigger.md)
- [Operator Notification and Intervention](./operator-wait-and-intervention.md)
- [External Trigger Capability And Providerless Ingress](./external-trigger-capability.md)
- [Waiting Plane And Reactivation](./waiting-plane-and-reactivation.md)

It also addresses part of the source-model discussion in issue `#380`:

- operator transport vs external participant channel
- direct runtime ingress vs tool-fetched external evidence
- where direct message input is allowed to enter the main queue

This RFC does not replace the external-trigger/providerless-ingress contract.
External trigger capability remains the integration ingress path.

## Core Judgment

Remote operator transport is an operator surface.

It is not:

- a generic `channel_event`
- a provider-specific chat bot inside Holon core
- a model tool such as `telegram_send_message`
- a permission bypass for arbitrary external senders

The runtime should admit a remote operator message as:

```ts
MessageEnvelope {
  kind: 'operator_prompt',
  origin: { kind: 'operator', actor_id: '...' },
  authority_class: 'operator_instruction',
  delivery_surface: 'remote_operator_transport',
  admission_context: 'operator_transport_authenticated',
  metadata: {
    operator_transport: { ... },
    reply_route: { ... }
  }
}
```

The exact enum additions can be finalized during implementation. The important
contract is that Holon preserves both:

- operator instruction authority
- remote transport provenance

## Authority Projection

This RFC does not redefine Holon's general provenance, admission, and authority
vocabulary. It applies the model from
[Provenance, Admission, and Authority](./default-trust-auth-and-control.md) to
the remote operator case.

A successfully admitted remote operator message projects to:

```ts
origin: { kind: 'operator', actor_id: 'operator:jolestar' }
delivery_surface: 'remote_operator_transport'
admission_context: 'operator_transport_authenticated'
authority_class: 'operator_instruction'
```

The remote transport does not create a new origin. It is the admission path by
which the operator reached Holon.

`operator_transport_authenticated` records that the transport binding was
validated. It does not by itself authorize every future tool action.

The remote operator ingress endpoint must not accept caller-supplied
`authority_class`. Holon derives `operator_instruction` from successful
operator transport admission.

Tool visibility remains profile/runtime/boundary derived, as defined in the
general RFC. Remote operator transport does not create a separate tool catalog.

## Non-Goals

This RFC does not define:

- a Telegram adapter
- a Slack adapter
- an IM connector marketplace
- group chat collaboration semantics
- provider-specific command menus or buttons
- rich mobile UI behavior
- a general notification workflow engine
- final execution/resource authorization policy

It also does not make ordinary external channel content operator-equivalent.

## Concepts

### 1. Operator Transport Binding

An operator transport binding is the bidirectional protocol object that maps a
remote provider identity to an operator actor, target agent, and outbound
delivery callback.

Suggested shape:

```ts
OperatorTransportBinding {
  binding_id: string
  transport: 'agentinbox' | string
  operator_actor_id: string
  target_agent_id: string
  default_route_id: string
  delivery_callback_url: string
  delivery_auth: {
    kind: 'hmac' | 'bearer'
    key_id?: string
  }
  capabilities: {
    text: boolean
    markdown?: boolean
    attachments?: boolean
  }
  provider: string
  provider_identity_ref: string
  status: 'active' | 'revoked' | 'expired'
  created_at: string
  last_seen_at?: string
  metadata?: Record<string, unknown>
}
```

Phase 1 should only support a fixed target agent.

Holon should not route remote operator messages through a mutable global
"current agent" concept. Long-lived runtimes can host multiple agents, so the
binding must make the target explicit.

The same binding is used for both directions:

- inbound authenticated remote operator messages
- outbound operator-facing delivery intents

Holon owns the protocol contract and runtime projection. The transport
implementation, such as AgentInbox, owns provider SDKs, provider conversation
state, and provider delivery lifecycle.

### 2. Reply Route

A reply route is an ephemeral delivery target attached to an inbound message.

Suggested shape:

```ts
ReplyRoute {
  kind: 'operator_transport'
  binding_id: string
  provider: string
  conversation_ref: string
  reply_to_message_id?: string
  metadata?: Record<string, unknown>
}
```

This route answers:

- where should the runtime send the user-facing result for this turn?

It should not answer:

- who has operator authority?
- what agent should receive future messages?
- what provider APIs should the model call?

Those are separate concerns.

### 3. Agent Delivery Policy

An agent delivery policy is the durable fallback for proactive or background
notifications that do not have a current inbound reply route.

Suggested shape:

```ts
AgentDeliveryPolicy {
  default_operator_route?: {
    kind: 'operator_transport'
    binding_id: string
  }
  notify_on: DeliveryTriggerKind[]
}

type DeliveryTriggerKind =
  | 'approval_requested'
  | 'operator_input_required'
  | 'turn_result'
  | 'work_item_done'
  | 'work_item_blocked'
  | 'task_result'
```

Phase 1 can keep this very small:

- one optional default operator route per agent
- a conservative built-in trigger set

## Inbound Admission

### Binding-based admission

A remote operator message may enter as `operator_prompt` only if:

- it comes through an active operator transport binding
- the binding resolves to an operator actor
- the binding resolves to exactly one target agent
- the provider identity matches the binding
- the transport adapter supplies enough provenance to audit the admission

If any of those fail, the message must not be promoted to operator input.

It may be:

- rejected
- preserved outside the main queue as tool-fetched `external_evidence`
- ignored while preserving an audit record

### Phase-1 scope

Phase 1 should only support private one-to-one operator transports.

Group chats and public channels should not be accepted as
`remote_operator_transport` in phase 1 because they complicate:

- sender identity
- participant authority
- message reply semantics
- accidental authority elevation

Group/public channel content belongs to tool-fetched evidence by default. This
RFC does not require preserving direct channel ingress for ordinary IM
messages.

## Message Envelope Projection

An admitted message should look conceptually like:

```ts
MessageEnvelope {
  agent_id: 'default',
  kind: 'operator_prompt',
  origin: { kind: 'operator', actor_id: 'jolestar' },
  authority_class: 'operator_instruction',
  priority: 'normal',
  body: { type: 'text', text: 'continue the previous task' },
  delivery_surface: 'remote_operator_transport',
  admission_context: 'operator_transport_authenticated',
  metadata: {
    operator_transport: {
      binding_id: 'opbind_123',
      provider: 'telegram',
      provider_identity_ref: 'telegram:user:...',
      conversation_ref: 'telegram:dm:...'
    },
    reply_route: {
      kind: 'operator_transport',
      binding_id: 'opbind_123',
      provider: 'telegram',
      conversation_ref: 'telegram:dm:...',
      reply_to_message_id: '456'
    }
  }
}
```

Provider-specific identifiers should be redacted or represented through stable
refs where needed. The runtime does not need to expose raw chat IDs to the
model.

## Continuation Semantics

Because remote operator input is still operator input, it should satisfy
`awaiting_operator_input`.

That is the main semantic difference from ordinary external evidence.

However, the runtime should still preserve the transport labels so status,
transcript, audit, and future policy can distinguish:

- local operator input
- TUI operator input
- remote operator transport input

Transport does not erase provenance.

## Outbound Delivery

## Core Rule

Outbound delivery is triggered by runtime user-facing output boundaries.

It should not be triggered by:

- arbitrary model text tokens
- internal tool traces
- provider-specific model tools
- provider adapters deciding on their own to mirror every event

The model should not need to know how to send a Telegram, Slack, or Matrix
message to reply to the operator.

## Delivery Router

Holon should introduce a delivery-router concept, even if the first
implementation is small.

The delivery router consumes user-facing runtime events and decides whether to
deliver them to an external route.

Important trigger points include:

- `approval_requested`
- `operator_input_required`
- terminal turn result / closure result
- work item done
- work item blocked
- task result that requires operator notification
- optional receipt acknowledgment

The delivery router should not treat provider delivery as part of model
execution success. Delivery is a user-facing side effect with its own audit
trail.

## Transport Delivery Callback

Outbound delivery should use the active operator transport binding's
`delivery_callback_url`.

Holon should not call an AgentInbox business-specific API. It should call the
standard operator transport delivery callback registered on the binding.

Suggested request:

```http
POST <delivery_callback_url>
Authorization: Bearer <delivery-token>
Idempotency-Key: <delivery_intent_id>
Content-Type: application/json
```

```json
{
  "delivery_intent_id": "odi_123",
  "binding_id": "opbind_123",
  "route_id": "route_123",
  "target_agent_id": "default",
  "kind": "operator_output",
  "text": "Done. Tests passed.",
  "created_at": "2026-04-23T00:00:00Z",
  "correlation_id": "...",
  "causation_id": "..."
}
```

The callback response should be interpreted narrowly:

- any 2xx response means the transport accepted the delivery intent
- `202 Accepted` is the preferred success response
- accepted by transport does not mean delivered to Telegram, Slack, or another
  upstream provider
- non-2xx or timeout means Holon failed to submit the delivery intent to the
  transport

Optional success response body:

```json
{
  "status": "accepted",
  "transport_delivery_id": "ain_del_456"
}
```

Phase 1 should not require an asynchronous provider-level delivery ack. The
transport implementation may record provider delivery attempts, retries, and
failures internally. Holon only needs to know whether the transport accepted
the delivery intent.

This keeps the dependency direction clean:

- Holon defines and stores the operator transport protocol object
- AgentInbox implements that protocol by registering a delivery callback URL
- Holon calls the registered callback when it has operator-facing output
- AgentInbox handles provider-specific delivery after accepting the intent

## Route Resolution Order

When a user-facing output needs delivery, resolve the target in this order:

1. inbound message `reply_route`
2. waiting intent or work-item delivery route
3. agent default operator route
4. local first-party surfaces such as event stream / TUI projection
5. no external route; persist only as brief/transcript/status

This order supports two different cases:

- replying to the operator who just sent a remote message
- proactively notifying the operator when background work finishes later

The runtime must not depend on "last message came from Telegram" as durable
state. Background work often completes outside the turn that created it.

## Delivery Records

Delivery attempts should be auditable.

A delivery record should capture at least:

- output id
- agent id
- route kind
- binding id when applicable
- trigger kind
- status
- transport delivery id when returned
- failure summary when delivery fails

Delivery failure should not rewrite the underlying turn or work outcome. A
task can complete successfully even if a remote notification fails.

Phase-1 statuses should distinguish at least:

- `pending`
- `accepted_by_transport`
- `failed_to_submit`

Do not model provider-level `delivered` or `read` states in Holon unless a
later RFC adds async transport acknowledgments.

## Provider Adapter Boundary

Provider-specific code should live outside Holon core or behind a narrow
adapter boundary.

The adapter may know:

- Telegram APIs
- Slack APIs
- Matrix APIs
- provider-specific formatting
- provider-specific message IDs

Holon core should know:

- operator transport binding
- reply route
- delivery route
- delivery intent submit status
- normalized message envelope

This keeps Holon aligned with its product boundary:

- runtime and control plane
- not connector marketplace

## HTTP Ingress Surface

Holon currently has multiple message ingress surfaces with different authority
semantics. Remote operator transport should not blur those boundaries.

### Public enqueue

`POST /agents/:agent_id/enqueue` is the public external ingress surface.

It is for:

- ordinary webhook events
- ordinary external channel events
- simple integration ingress that is not using callback capability

It is not for:

- operator prompts
- operator authority assertions
- provider identity promotion
- remote operator reply routes
- control-plane mutations

This endpoint should continue to reject attempts to submit operator origin or
assert operator authority. That protects Holon from a public surface silently
becoming an operator authority surface.

### Local control prompt

`POST /control/agents/:agent_id/prompt` remains the local first-party operator
prompt surface.

It is appropriate for:

- CLI prompt input
- TUI prompt input
- local control clients

It is intentionally narrow. It should not be overloaded with AgentInbox or
provider-specific metadata because the endpoint currently represents local
control input, not a remote authenticated operator transport.

### Remote operator ingress

Phase 1 should add a dedicated authenticated control endpoint:

```http
POST /control/agents/:agent_id/operator-ingress
Authorization: Bearer <control-token>
```

Suggested request shape:

```json
{
  "text": "continue the previous task",
  "actor_id": "operator:jolestar",
  "binding_id": "opbind_123",
  "reply_route_id": "route_123",
  "provider": "agentinbox",
  "upstream_provider": "telegram",
  "provider_message_ref": "telegram:message:456",
  "correlation_id": "ain_789",
  "causation_id": null,
  "metadata": {
    "conversation_ref": "telegram:dm:..."
  }
}
```

The endpoint should project the request into a normal runtime message:

```ts
MessageEnvelope {
  agent_id: 'default',
  kind: 'operator_prompt',
  origin: { kind: 'operator', actor_id: 'operator:jolestar' },
  authority_class: 'operator_instruction',
  priority: 'normal',
  body: { type: 'text', text: 'continue the previous task' },
  delivery_surface: 'remote_operator_transport',
  admission_context: 'operator_transport_authenticated',
  metadata: {
    operator_transport: {
      provider: 'agentinbox',
      binding_id: 'opbind_123',
      reply_route_id: 'route_123',
      upstream_provider: 'telegram',
      provider_message_ref: 'telegram:message:456',
      conversation_ref: 'telegram:dm:...'
    }
  },
  correlation_id: 'ain_789'
}
```

The endpoint is still a control endpoint. Holon accepts AgentInbox's operator
transport assertion only because AgentInbox authenticated the remote provider
identity and Holon authenticated AgentInbox as a control-plane caller.

It should not accept arbitrary provider claims from unauthenticated public
traffic.

### Required enum additions

The implementation should add explicit labels instead of reusing the local
control prompt labels:

```rust
MessageDeliverySurface::RemoteOperatorTransport
AdmissionContext::OperatorTransportAuthenticated
AuthorityClass::OperatorInstruction
```

This keeps audit, transcript, TUI projection, and future policy able to
distinguish:

- local control prompt
- public external enqueue
- callback capability
- authenticated remote operator transport

The endpoint should not accept caller-supplied `authority_class`; it should
derive `operator_instruction` from successful operator transport admission.

## AgentInbox-Backed Profile

AgentInbox is the recommended first implementation profile for remote operator
transport.

In this profile, AgentInbox owns:

- provider SDKs and protocol sessions
- provider user identity normalization
- operator transport binding and revocation
- provider conversation and message refs
- stable reply route ids
- the transport delivery callback implementation
- provider send attempts and provider send status

Holon owns:

- operator transport binding records
- runtime message projection
- origin/admission/authority labels
- queueing and continuation semantics
- waiting/work-item satisfaction
- user-facing output events
- delivery intent selection

Inbound flow:

```text
IM provider DM
  -> AgentInbox provider adapter
  -> AgentInbox resolves active operator binding
  -> AgentInbox calls Holon operator-ingress
  -> Holon enqueues operator_prompt
```

Outbound flow:

```text
Holon user-facing output event
  -> Holon resolves reply/default route
  -> Holon POSTs delivery intent to binding.delivery_callback_url
  -> AgentInbox returns 202 Accepted when the intent is accepted
  -> AgentInbox sends provider message
  -> AgentInbox records provider delivery attempt
```

Holon should store only stable binding and route references needed for audit
and routing. Provider-native identifiers should stay in AgentInbox unless they
are needed as redacted audit metadata.

The binding's `delivery_callback_url` is the outbound integration point. Holon
does not need a hard-coded AgentInbox delivery client; it only submits the
standard operator transport delivery-intent payload to the callback URL
registered by the binding.

## Relationship To External Channel Content

Remote operator transport and ordinary external channel content are different
concepts.

Remote operator transport:

- is bound to the primary operator
- enters as `operator_prompt`
- has `authority_class = operator_instruction`
- may satisfy a future explicit operator wait
- replies can use the inbound `reply_route`

External channel content:

- comes from an external communication surface such as a group chat, public
  channel, issue comment, review comment, or email thread
- should normally be fetched through tools or source/inbox inspection rather
  than directly enqueued into the main runtime queue
- has `authority_class = external_evidence`
- should not satisfy operator wait
- should not override operator instructions

Do not collapse these two concepts just because both might use Telegram.

## Relationship To External Trigger Capability

External trigger capability remains the integration path.

External trigger capability is for:

- external systems
- AgentInbox
- CI
- GitHub
- providerless wake/re-entry flows

Remote operator transport is for:

- the operator sending direct input
- the runtime delivering user-facing output back to the operator

Both can coexist, but they should not share authority semantics.

## Phase-1 Recommendation

Phase 1 should implement the smallest useful contract:

- support one-to-one private operator transport bindings only
- bind each remote operator identity to exactly one target agent
- admit bound messages as `operator_prompt`
- attach transport provenance and an inbound `reply_route`
- store a transport `delivery_callback_url` on the binding
- automatically deliver terminal user-facing turn results to the inbound
  `reply_route` through the binding delivery callback
- deliver background/task/work-item notifications to the agent default
  operator route when configured
- treat 2xx callback responses as `accepted_by_transport`, not provider-level
  delivery
- persist brief/transcript/status when no external delivery route is available
- keep provider-specific adapters outside the core runtime contract

Phase 1 should not support:

- group chat as operator transport
- multiple active target agents per binding
- model-visible Telegram or Slack send tools for operator replies
- provider-specific command menus as runtime contract
- unbound channel messages becoming operator input

## Open Questions

- Should the enum names be `remote_operator_transport` and
  `operator_transport_authenticated`, or should Holon use shorter names such
  as `operator_remote`?
- Should an operator transport binding be stored as agent state, actor state,
  or a separate control-plane object?
- Should route switching such as `/use agent-id` be a future operator transport
  control command, or should each binding remain fixed forever?
- How much provider metadata should be visible to the model versus only to
  audit/status surfaces?
- Should delivery failure create a model-visible follow-up, an operator-facing
  status only, or both?
- Should approval delivery reuse the same delivery router or define a narrower
  approval-specific route family?

## Summary

Holon should treat remote operator transport as a first-class operator surface
with preserved transport provenance.

Inbound remote operator messages should enter the target agent queue as
operator prompts only after binding-based authentication. Outbound replies
should be emitted by the runtime delivery router from user-facing output
events, not by model-visible provider send tools.

This keeps Holon's authority boundary clear while still making IM and mobile
operator workflows possible.
