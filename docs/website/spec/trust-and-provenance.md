---
title: Trust and provenance
summary: Current provenance, admission/authentication, instruction authority, and execution policy contract.
order: 80
---

# Trust and provenance

This page defines the current contract for how Holon classifies message
provenance, admission/authentication, instruction authority, and execution
policy — and how those labels flow through the runtime.

> **Last verified:** 2026-05-27 against `src/types.rs` `MessageEnvelope`,
> `MessageOrigin`, `AuthorityClass`, `MessageDeliverySurface`,
> `AdmissionContext`, `src/policy.rs`, `src/ingress.rs`, `src/http/mod.rs`,
> `src/context/mod.rs`, `src/prompt/mod.rs`, `src/operator_event.rs`,
> `src/presentation.rs`, `src/runtime/message_dispatch.rs`, and
> `src/runtime/operator_dispatch.rs`.

## Source RFCs

- [Provenance, Admission, and Authority](https://github.com/holon-run/holon/blob/main/docs/rfcs/default-trust-auth-and-control.md)
- [Event Stream Interface Design](https://github.com/holon-run/holon/blob/main/docs/rfcs/event-stream-interface.md)
- [Remote Operator Transport and Delivery](https://github.com/holon-run/holon/blob/main/docs/rfcs/remote-operator-transport-and-delivery.md)
- [Operator Display Levels and Event Presentation](https://github.com/holon-run/holon/blob/main/docs/rfcs/operator-display-levels-and-event-presentation.md)
- [Tool Surface Layering](https://github.com/holon-run/holon/blob/main/docs/rfcs/tool-surface-layering.md)
- [Continuation Trigger](https://github.com/holon-run/holon/blob/main/docs/rfcs/continuation-trigger.md)

## Message envelope

Every queued message in Holon carries provenance, admission, authority, and
scheduling labels in its `MessageEnvelope`:

```text
MessageEnvelope {
    origin: MessageOrigin,
    authority_class: AuthorityClass,
    priority: Priority,
    delivery_surface: Option<MessageDeliverySurface>,
    admission_context: Option<AdmissionContext>,
    trigger_kind: Option<ContinuationTriggerKind>,
    source_refs: BTreeMap<String, String>,
    ...
}
```

The four concepts below are intentionally separate:

- **Provenance** answers who or what produced the content (`origin`) and
  preserves correlated source identifiers (`source_refs`).
- **Admission/authentication** answers how and why Holon accepted the message
  (`delivery_surface` and `admission_context`).
- **Instruction authority** answers whether the content is an operator
  instruction, runtime directive, integration signal, or external evidence
  (`authority_class`).
- **Execution policy** answers whether a concrete tool or resource action is
  allowed in the current execution boundary. It consumes these labels but is
  not replaced by any single label.

## Provenance: `MessageOrigin` and source refs

`MessageOrigin` captures the producer of the content. It does not encode how
the ingress was authenticated and does not by itself grant tool authority.

| Origin variant | Current meaning | Typical kind |
|----------------|-----------------|--------------|
| `Operator { actor_id }` | Direct operator-authored content. The admission surface may be local CLI, run-once, HTTP control, or remote operator transport. | `OperatorPrompt`, `Control` |
| `Channel { channel_id, sender_id }` | External channel content. Public enqueue accepts this as external evidence by default. | `ChannelEvent` |
| `Webhook { source, event_type }` | External webhook content. Public enqueue defaults to this origin when no origin is supplied. | `WebhookEvent` |
| `Callback { descriptor_id, source }` | External trigger callback admitted by a capability secret. The body is an integration signal, not an operator instruction. | `CallbackEvent` |
| `Timer { timer_id }` | Scheduled timer fire. | `TimerTick` |
| `System { subsystem }` | Runtime-owned internal message, such as scheduler, lifecycle, or internal follow-up. | `SystemTick`, `InternalFollowup`, `Control` |
| `Task { task_id }` | Task status/result from a supervised command or child agent. | `TaskStatus`, `TaskResult` |

`MessageEnvelope::normalize_admission_fields` derives `trigger_kind`, `task_id`
for task-origin messages, and `source_refs` such as `task_id`, `timer_id`,
`external_trigger_id`, `waiting_intent_id`, `callback_delivery_id`, and
`queued_event_id`. Binding fields such as `work_item_id` and `task_id` are
projected from metadata only for runtime-owned messages admitted through
`RuntimeSystem` or `TaskRejoin`; untrusted external metadata remains evidence.

## Admission/authentication: delivery surface and admission context

`MessageDeliverySurface` records where the message entered or was produced by
the runtime:

| Delivery surface | Current use |
|------------------|-------------|
| `CliPrompt` | Local interactive prompt input. |
| `RunOnce` | Local one-shot run input. |
| `HttpPublicEnqueue` | Public HTTP enqueue after remote access admission; cannot request `Interject` priority or override authority. |
| `HttpWebhook` | HTTP webhook transport. |
| `HttpCallbackEnqueue` | Callback endpoint that enqueues a message. |
| `HttpCallbackWake` | Callback endpoint used as a wake hint. |
| `HttpControlPrompt` | Authenticated HTTP control prompt. |
| `RemoteOperatorTransport` | Authenticated remote operator transport. |
| `TimerScheduler` | Runtime timer scheduler. |
| `RuntimeSystem` | Runtime-owned system surface. |
| `TaskRejoin` | Task supervisor result/status rejoin. |

`AdmissionContext` records why Holon accepted the ingress:

| Admission context | Current meaning |
|-------------------|-----------------|
| `PublicUnauthenticated` | Public enqueue/webhook-style input admitted without operator credentials. |
| `ControlAuthenticated` | Control-plane request admitted by the configured control token. |
| `OperatorTransportAuthenticated` | Remote operator transport authenticated as an operator surface. |
| `ExternalTriggerCapability` | Callback admitted by possession of an external trigger capability secret. |
| `LocalProcess` | Local process or local control surface; current host-local execution policy still applies. |
| `RuntimeOwned` | Message produced by the runtime itself. |

Admission is not the same as instruction authority. For example, an
`ExternalTriggerCapability` proves that a callback URL was valid, but the
callback payload remains an `IntegrationSignal`, not an `OperatorInstruction`.

## Authority class (`AuthorityClass`)

`AuthorityClass` is the current instruction-authority vocabulary:

| Class | Meaning | Default origins |
|-------|---------|-----------------|
| `OperatorInstruction` | Operator-authored instruction the agent should follow, subject to instruction precedence and execution policy. | `Operator` |
| `RuntimeInstruction` | Runtime-owned directive or lifecycle/task signal. | `System`, `Task`, `Timer` |
| `IntegrationSignal` | Configured integration or callback signal. It may wake or inform work but is not an operator instruction. | `Webhook`, `Callback` |
| `ExternalEvidence` | External channel content for inspection. | `Channel` |

**Key contract:**

- `AuthorityClass` is assigned by ingress/runtime code and is not reassigned by
  the model.
- Public enqueue may not override `authority_class`; trusted ingress may supply
  it or default it from origin.
- `validate_message_kind_for_origin` admits only origin/kind combinations that
  match the runtime contract.
- Operator input and external channel input must not be merged without
  preserving provenance.
- The prompt tells the model to treat external or lower-authority payloads as
  evidence, not operator-equivalent instruction.

### Transitional `trust` wording

The earlier `trust` / `trusted_*` / `untrusted_*` vocabulary is not the primary
public contract. Current code keeps compatibility in two places:

- `MessageEnvelope` deserialization accepts legacy `trust` as an alias for
  `authority_class`.
- `AuthorityClass` variants accept old serde/CLI aliases such as
  `trusted_operator`, `trusted_system`, `trusted_integration`, and
  `untrusted_external`.

[`src/context/mod.rs`](../../../src/context/mod.rs) still renders a `trust=`
label in model context for backward
readability, but it is derived from `authority_class`. New docs and new
contracts should name `authority_class` directly.

## Current classification matrix

| Producer / path | Origin | Kind | Authority | Delivery surface | Admission context |
|-----------------|--------|------|-----------|------------------|-------------------|
| Local operator prompt | `Operator` | `OperatorPrompt` | `OperatorInstruction` | `CliPrompt` / `RunOnce` | `LocalProcess` |
| HTTP control prompt with token | `Operator` | `OperatorPrompt` or `Control` | `OperatorInstruction` | `HttpControlPrompt` | `ControlAuthenticated` |
| Remote operator transport | `Operator` | `OperatorPrompt` | `OperatorInstruction` | `RemoteOperatorTransport` | `OperatorTransportAuthenticated` |
| Public external channel enqueue | `Channel` | `ChannelEvent` | `ExternalEvidence` | `HttpPublicEnqueue` | `PublicUnauthenticated` |
| Public webhook enqueue | `Webhook` | `WebhookEvent` | `IntegrationSignal` | `HttpPublicEnqueue` / `HttpWebhook` | `PublicUnauthenticated` |
| External callback enqueue/wake | `Callback` | `CallbackEvent` | `IntegrationSignal` | `HttpCallbackEnqueue` / `HttpCallbackWake` | `ExternalTriggerCapability` |
| Timer fire | `Timer` | `TimerTick` | `RuntimeInstruction` | `TimerScheduler` | `RuntimeOwned` |
| Runtime system/internal follow-up | `System` | `SystemTick` / `InternalFollowup` / `Control` | `RuntimeInstruction` | `RuntimeSystem` | `RuntimeOwned` |
| Task status/result | `Task` | `TaskStatus` / `TaskResult` | `RuntimeInstruction` | `TaskRejoin` | `RuntimeOwned` |

## Priority

| Priority | Scheduling effect |
|----------|-------------------|
| `Interject` | Preempts queued work; delivered before normal-priority messages |
| `Next` | Delivered after current interject messages, before `Normal` |
| `Normal` | Standard queue position |
| `Background` | Low-urgency; delivered after higher-priority messages |

Priority affects queue ordering, not authority, admission, or execution policy.

## Execution policy

Execution policy is the final allow/deny boundary for concrete process, file,
network, message ingress, control-plane, workspace projection, and agent-state
actions. It is summarized to the model as the current execution policy snapshot
and enforced by runtime/tool surfaces where hard enforcement exists.

Authority labels inform execution policy but do not replace it. For example,
`OperatorInstruction` can request an action, but the tool still runs under the
current workspace projection, process-execution, secret-isolation, and
path/write/network policy. Conversely, `RuntimeInstruction` may carry lifecycle
state, but it does not make arbitrary external payload text trustworthy.

## Provenance preservation and exposure

Holon's provenance contract:

- Origin, authority, delivery surface, admission context, trigger kind, source
  refs, correlation id, and causation id are **never reassigned** by the model.
- When the runtime generates internal messages (system ticks, task results,
  timer fires, continuation follow-ups), it assigns runtime-owned origin and
  `RuntimeInstruction` authority.
- When an agent delegates work via `SpawnAgent`, the child agent receives the
  delegated task as bounded operator/runtime context according to the supervising
  runtime surface; it must not silently merge later external channel content into
  operator instruction.
- Model context renders the current message's origin, authority, delivery
  surface, admission context, and legacy derived trust label. Operator
  interjections include explicit `origin`, `authority_class`, `delivery_surface`,
  and `admission_context` metadata in the turn prompt.
- TUI and first-party presentation use raw projection/runtime events and reduce
  them client-side. User-message presentation renders only messages whose origin
  is `Operator`; external events do not become user chat messages merely because
  they contain text.
- HTTP event streams expose raw runtime events and message payloads, including
  provenance labels on message admission, processing, and transcript events.
- User-facing summaries should summarize results, blockers, and requested
  operator actions without discarding the underlying message/event provenance
  from the durable runtime/event stream.

## Validation

Validated implementation points:

- `src/types.rs` defines the canonical envelope fields and legacy alias
  compatibility.
- `src/policy.rs` defines default authority by origin and kind/origin admission
  checks.
- [`src/http/mod.rs`](../../../src/http/mod.rs) prevents public enqueue from
  using runtime-owned kinds,
  `Interject` priority, privileged origins, or authority overrides.
- [`src/context/mod.rs`](../../../src/context/mod.rs) and
  [`src/prompt/mod.rs`](../../../src/prompt/mod.rs) expose
  authority/provenance labels to the model and preserve the external-evidence
  trust boundary in prompt instructions.
- [`src/runtime/message_dispatch.rs`](../../../src/runtime/message_dispatch.rs),
  [`src/runtime/operator_dispatch.rs`](../../../src/runtime/operator_dispatch.rs),
  and [`src/runtime/turn/execution.rs`](../../../src/runtime/turn/execution.rs)
  include provenance labels in queue, processing, transcript, and interjection
  events.
- `src/operator_event.rs` and `src/presentation.rs` keep raw event provenance
  available while rendering operator-origin messages as user messages.

## Drift and follow-up classification

- **Stale RFC wording:** `docs/rfcs/default-trust-auth-and-control.md` says
  `TrustLevel` may remain as a transitional implementation detail. The current
  implementation has already removed the `TrustLevel` enum/field from the
  public `MessageEnvelope`; only legacy aliases and a derived model-context
  `trust` label remain.
- **Unresolved design decision:** `signed_integration` appears in the RFC as a
  possible admission context, but there is no `AdmissionContext::SignedIntegration`
  variant yet.
- **Missing test coverage addressed here:** `src/policy.rs` now has a direct
  classification matrix test for all current origin defaults and allowed
  origin/kind pairs.
- **No implementation bug found:** the inspected ingress, prompt, event, and
  presentation paths preserve the current provenance/authority separation.
