---
title: Wake and continuation
summary: Current trigger classification, external ingress capabilities, continuation resolution, and wake/sleep lifecycle.
order: 40
---

# Wake and continuation

This page defines the current contract for how Holon agents wake from sleep,
receive external events, and resolve continuation decisions.

> **Last verified:** 2026-05-23 against `src/types.rs`
> `ContinuationTriggerKind`, `ContinuationClass`, `ContinuationResolution`,
> `PendingWakeHint`, `ExternalTriggerScope`, `CallbackDeliveryMode`,
> `ExternalTriggerSummary`, `ExternalTriggerCapability`,
> `WaitingIntentRecord`, and `src/runtime/waiting.rs`.

## Source RFCs

- [Continuation Trigger](https://github.com/holon-run/holon/blob/main/docs/rfcs/continuation-trigger.md)
- [External Trigger Capability And Providerless Ingress](https://github.com/holon-run/holon/blob/main/docs/rfcs/external-trigger-capability.md)
- [Waiting Plane And Reactivation](https://github.com/holon-run/holon/blob/main/docs/rfcs/waiting-plane-and-reactivation.md)
- [Operator Wait And Intervention](https://github.com/holon-run/holon/blob/main/docs/rfcs/operator-wait-and-intervention.md)
- [Remote Operator Transport and Delivery](https://github.com/holon-run/holon/blob/main/docs/rfcs/remote-operator-transport-and-delivery.md)
- [Event Stream Interface Design](https://github.com/holon-run/holon/blob/main/docs/rfcs/event-stream-interface.md)

## Trigger classification

When an agent is reactivated, the continuation is classified by trigger kind
and class:

### Trigger kinds (`ContinuationTriggerKind`)

| Kind | Source |
|------|--------|
| `OperatorInput` | Direct operator message (CLI, HTTP, TUI) |
| `TaskResult` | A command task or child-agent task reached a terminal state |
| `ExternalEvent` | An external system sent a contentful event via ingress URL |
| `TimerFire` | A scheduled timer elapsed |
| `InternalFollowup` | The agent enqueued a self-follow-up via `Enqueue` |
| `SystemTick` | The scheduler emitted a runtime-owned follow-up for a runnable WorkItem |

`SystemTick` is also how Holon resumes other runnable WorkItems after a
promoted `CompleteWorkItem` report ends the current turn.

### Continuation classes (`ContinuationClass`)

| Class | Meaning |
|-------|---------|
| `ResumeExpectedWait` | The trigger matched the prior waiting reason exactly |
| `ResumeOverride` | The trigger overrides the prior wait (e.g., operator interrupt) |
| `LocalContinuation` | The agent continued without sleeping (same-turn follow-up) |
| `LivenessOnly` | Wake hint — does not carry model-visible content |

## Wake hints vs contentful events

**Wake hints** are liveness signals. They tell the scheduler "something changed
externally, re-evaluate whether the agent should wake". The hint payload is
**not** delivered as a model-visible message. The agent must query the
external system for details after waking.

**Contentful external events** carry an `enqueue_message` delivery mode. The
event payload is delivered as a message in the agent's queue with provenance
preserved.

| Delivery mode | Behavior |
|---------------|----------|
| `WakeHint` | Liveness signal; scheduler emits `EmitSystemTick` |
| `EnqueueMessage` | Full event payload enqueued as a message |

## External trigger capabilities

Holon provisions a **default external ingress capability** for each agent at
initialization. This is a capability URL with a secret token:

- The URL is exposed in the agent's context as `default_external_ingress`.
- External systems POST to this URL to deliver wake hints or events.
- The capability is agent-scoped; it can be reused across WorkItems.
- Capability revocation is an administrative action, not a per-cycle model
  tool call.

### Capability model

| Field | Purpose |
|-------|---------|
| `external_trigger_id` | Unique trigger identifier |
| `trigger_url` | The ingress URL (capability secret) |
| `target_agent_id` | The agent this trigger wakes |
| `delivery_mode` | `WakeHint` or `EnqueueMessage` |
| `scope` | `Agent` |
| `status` | `Active` or `Revoked` |

External triggers are provisioned at agent initialization. Agents do not create
or cancel triggers at runtime.

## Continuation resolution

When the agent wakes, `ContinuationResolution` records how the activation
occurred:

| Field | Meaning |
|-------|---------|
| `trigger_kind` | Which kind of event caused the wake |
| `class` | How the wake relates to the prior wait |
| `model_reentry` | Whether the model should be re-entered with context |
| `prior_closure_outcome` | What the prior turn decided |
| `prior_waiting_reason` | What the agent was waiting for |
| `matched_waiting_reason` | Whether the trigger matched the reason |

**Key contract:**

- `model_reentry=true` means the model receives continuation context including
  the prior closure and trigger evidence.
- `model_reentry=false` means the wake is a liveness check; the scheduler
  re-evaluates posture but may not need to re-enter the model.
- `matched_waiting_reason` distinguishes "expected wake" from "surprise
  wake". An operator message arriving while waiting for a task result does not
  match, and the agent may need to handle the interruption.

## Known gaps

- `WaitingIntentRecord` retains an internal `scope` field for scheduler
  accounting of agent-level versus WorkItem-bound waiting state. External
  trigger capabilities themselves are agent-scoped and partitioned only by
  delivery mode.
- `WakeHint` idempotency is implemented via `PendingWakeHint` deduplication
  but the contract for when duplicate hints are silently dropped vs surfaced
  as diagnostics is not yet a stable API.
- `EnqueueMessage` delivery for external events preserves provenance but
  the provenance taxonomy for external sources is not yet fully specified.
