# Callback Capability And Providerless Ingress

Canonical RFC: `docs/rfcs/external-trigger-capability.md`.

This note uses the earlier callback/reactivation vocabulary. New RFCs and
model-facing documentation should prefer **External Trigger Capability**,
`CreateExternalTrigger`, and `CancelExternalTrigger`.

This note captures a providerless direction for external event integration in
Holon. **This design is implemented and partially shipped.**

The design goal is:

- Holon core should not need to know GitHub, Slack, email, CI, or any other
  provider-specific concepts
- agents should still be able to express "wake me when this condition becomes
  true"
- new integrations should be added without changing core runtime semantics

## Core Idea

Instead of teaching Holon about every external provider, give the agent a
callback capability.

Then:

1. the agent discovers an external tool or skill that can register an event
   subscription
2. the agent asks Holon for a callback capability describing how to wake it
3. the agent gives that callback capability to the external tool
4. the external tool registers the actual watch with the outside system
5. when the condition fires, the external tool pushes an event back to Holon
6. Holon turns that push into a normal queued message for the target agent

**Implementation status: Core shipped, expiry tracking not yet implemented.**

In this model:

- Holon does not know the provider
- Holon does not define provider-specific watch schemas
- Holon only provides a safe wakeup endpoint

## What The Agent Needs

The agent needs some self-knowledge, but not raw implementation details.

The right abstraction is not:

- "what local port am I running on?"

The better abstraction is:

- "give me a callback capability I can hand to an outside system"

**Implementation status:** The `CreateExternalTrigger` tool is shipped and
returns an `ExternalTriggerCapability`:

```ts
ExternalTriggerCapability {
  waiting_intent_id: string
  external_trigger_id: string
  trigger_url: string
  target_agent_id: string
  delivery_mode: 'wake_only' | 'enqueue_message'
}
```

This is much better than exposing a bare port number.

## Why This Is Attractive

This keeps Holon aligned with its likely role:

- not a connector platform
- not a provider taxonomy registry
- not a webhook business-logic hub

Instead Holon becomes:

- a runtime that can be safely woken by external systems

That is a much cleaner long-term substrate.

## Why Not Expose Just A Port

A raw port is too low-level and too brittle.

It creates obvious problems:

- weak authentication model
- awkward local-vs-remote deployment differences
- difficult future migration
- encourages agents to reason about transport details instead of runtime
  capabilities

**Implementation status:** The shipped implementation uses a token-based callback URL with:
- Cryptographically secure tokens
- Per-descriptor revocation
- Delivery mode enforcement

A callback capability is safer and more portable.

It lets Holon change its transport details later while preserving the higher
level contract.

## What The External Tool Does

The external tool or skill is responsible for provider-specific logic.

For example:

- registering a webhook
- creating a PR-status watcher
- waiting for an email reply
- tracking CI completion
- subscribing to a message bus

That tool may be implemented as:

- a skill-backed tool
- a plugin
- an MCP service
- a separate worker process

From Holon's perspective, it is just an external actor that later calls back
with a signed event.

## What Holon Core Needs To Do

Holon core only needs a few generic abilities. **Core shipped, expiry tracking not yet implemented.**

### 1. Produce callback capabilities

The runtime can mint a scoped external trigger for a target agent via
`CreateExternalTrigger`.

### 2. Receive callback pushes

The HTTP ingress exposes a generic callback endpoint that accepts callback deliveries.

### 3. Validate and normalize

When a callback arrives, Holon validates:

- token or capability secret
- target agent
- allowed wake behavior

Then normalizes it into a standard queued message.

### 4. Re-enter through the queue

Even in this model, the callback does not directly trigger arbitrary code.

It becomes a normal queued event for the target agent.

This preserves:

- origin
- trust
- auditability
- wake reason
- scheduling consistency

## Relationship To Skills

This can use a skill-like extension mechanism, but the important thing is not
the packaging format.

The important thing is the separation of concerns:

- Holon core provides callback capability + ingress
- the extension provides provider-specific subscription semantics

So this is not just a prompt skill.

It is better thought of as:

- a skill-backed external capability
- or an integration capability built on the same extension substrate

## Example Flow

Suppose an agent wants to wait for a PR review.

The flow could be:

1. the agent calls a Holon tool to request a callback capability
2. Holon returns:
   - callback URL
   - token
   - target agent id
3. the agent calls an external GitHub-capable skill/tool
4. that tool registers a watch in its own provider-specific way
5. when review arrives, the tool POSTs to Holon's callback URL
6. Holon validates the request and enqueues a normal event for the agent

At no point does Holon need to understand:

- pull request schema
- review states
- GitHub webhook shape

It only understands:

- capability validation
- target agent routing
- queue wakeup

## Important Boundaries

This design still needs strong rules. **Core rules enforced, expiry tracking not yet implemented.**

### Rule 1: Callback delivery must be data-only

The external caller should submit an event payload, not executable behavior.

**Implementation:** Callback ingress accepts `text` or `json` payloads, not executable code.

### Rule 2: Every callback must resolve to a target agent

No broadcast-by-default behavior.

**Implementation:** Every external trigger is tied to a specific `target_agent_id`.

### Rule 3: Trigger behavior must be explicit

The callback capability should specify whether delivery:

- only queues a message (`enqueue_message`)
- or only wakes the target (`wake_only`)

**Implementation:** `delivery_mode` is enforced. `wake_only` callbacks create wake hints (may become `SystemTick`), while `enqueue_message` callbacks add structured content to the queue.

### Rule 4: Capabilities should be revocable

Agents need a way to cancel callbacks that are no longer needed.

**Implementation:** External triggers track `created_at` and can be cancelled
via `CancelExternalTrigger`. Expiry (time-based automatic revocation) is not yet
implemented.

### Rule 5: Holon should preserve provenance

The resulting message should retain enough metadata to answer:

- who delivered this
- why it was accepted
- which callback capability authorized it

**Implementation:** Messages from callbacks include `origin = Callback` with
metadata tracking `waiting_intent_id`, `external_trigger_id`,
`external_trigger_id`, `source`, and `resource`.

## Deployment Caveat

There is one practical caveat:

An outside SaaS often cannot reach a purely local runtime directly.

So in practice, this model may require:

- a public relay
- a bridge
- a tunnel
- or a hosted ingress service

That does not invalidate the design.

It only means the callback capability may eventually point to:

- a relay-backed endpoint

Rather than:

- a directly reachable local port

**Implementation note:** The current implementation uses `callback_base_url` from config, allowing deployment-specific URL schemes.

## Recommendation

If Holon wants to remain provider-agnostic, the cleanest design is:

- do not teach core about providers
- do not define provider-specific watch types in core
- do teach core how to mint callback capabilities
- do teach core how to accept and normalize external wake events

That gives Holon a minimal but powerful external-ingress substrate.

## Short Version

- Let agents obtain a callback capability, not a raw port. Implemented via
  `CreateExternalTrigger`.
- Let external tools own provider-specific subscription logic.
- Let those tools call back into Holon when conditions are met. Implemented via HTTP ingress
- Holon stays providerless and simply turns callback deliveries into normal
  queued wake events.
- **Implementation status:** Core functionality shipped, expiry tracking not yet implemented.
