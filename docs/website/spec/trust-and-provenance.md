---
title: Trust and provenance
summary: Current origin classification, admission/authentication, authority, and provenance tracking contract.
order: 80
---

# Trust and provenance

This page defines the current contract for how Holon classifies message
origin, trust, authority, and provenance — and how those labels flow through
the runtime.

> **Last verified:** 2026-05-23 against `src/types.rs` `MessageEnvelope`,
> `MessageOrigin`, `TrustLevel`, `AuthorityClass`, `Priority`,
> `AdmissionContext`, and `src/ingress.rs`.

## Source RFCs

- [Provenance, Admission, and Authority](https://github.com/holon-run/holon/blob/main/docs/rfcs/default-trust-auth-and-control.md)
- [Event Stream Interface Design](https://github.com/holon-run/holon/blob/main/docs/rfcs/event-stream-interface.md)
- [Remote Operator Transport and Delivery](https://github.com/holon-run/holon/blob/main/docs/rfcs/remote-operator-transport-and-delivery.md)
- [Operator Display Levels and Event Presentation](https://github.com/holon-run/holon/blob/main/docs/rfcs/operator-display-levels-and-event-presentation.md)
- [Tool Surface Layering](https://github.com/holon-run/holon/blob/main/docs/rfcs/tool-surface-layering.md)
- [Continuation Trigger](https://github.com/holon-run/holon/blob/main/docs/rfcs/continuation-trigger.md)

## Message envelope

Every message in Holon carries provenance labels in its `MessageEnvelope`:

```text
MessageEnvelope {
    origin: MessageOrigin,
    trust: TrustLevel,
    authority_class: AuthorityClass,
    priority: Priority,
    delivery_surface: Option<MessageDeliverySurface>,
    admission_context: Option<AdmissionContext>,
    ...
}
```

## Origin (`MessageOrigin`)

| Origin variant | Example |
|----------------|---------|
| `Operator { actor_id }` | Direct operator input via CLI, HTTP, TUI |
| `Channel { channel_id, sender_id }` | Message from an external channel |
| `Webhook { source, event_type }` | External webhook callback |
| `Callback { descriptor_id, source }` | Legacy callback trigger |
| `Timer { timer_id }` | Scheduled timer fire |
| `System { subsystem }` | Runtime-owned internal message |
| `Task { task_id }` | Task result from a background command or child agent |

## Trust level (`TrustLevel`)

| Level | Meaning | Typical origin |
|-------|---------|----------------|
| `TrustedOperator` | Authenticated operator with full authority | `Operator` with valid auth |
| `TrustedSystem` | Runtime-owned components (scheduler, task supervisor) | `System`, `Task` |
| `TrustedIntegration` | Configured external system with a known identity | `Webhook` from configured source |
| `UntrustedExternal` | Unknown or unauthenticated external source | `Webhook` from unknown source |

**Key contract:**

- Trust is assigned at ingress, not by the model.
- `TrustedOperator` messages carry instruction authority.
- `UntrustedExternal` messages are treated as evidence, not instructions.
- The model must not escalate trust based only on message content.

## Authority class (`AuthorityClass`)

| Class | Meaning |
|-------|---------|
| `OperatorInstruction` | This message is an instruction the agent should follow |
| `RuntimeInstruction` | This message is a runtime-owned directive (system tick, task result) |
| `IntegrationSignal` | This message is a signal from a configured integration |
| `ExternalEvidence` | This message is untrusted external content for inspection |

**Key contract:**

- `AuthorityClass` is separate from `TrustLevel`. A `TrustedIntegration` message
  may carry `IntegrationSignal` authority (not `OperatorInstruction`).
- The model receives both trust and authority labels and must respect the
  distinction.
- Operator input and external channel input must not be merged without
  preserving provenance.

## Priority

| Priority | Scheduling effect |
|----------|-------------------|
| `Interject` | Preempts queued work; delivered before normal-priority messages |
| `Next` | Delivered after current interject messages, before `Normal` |
| `Normal` | Standard queue position |
| `Background` | Low-urgency; delivered after higher-priority messages |

Priority affects queue ordering, not trust or authority.

## Admission context

`AdmissionContext` records how a message entered the runtime, including the
transport (HTTP endpoint, CLI stdin, TUI), authentication method, and any
gateway metadata. It is preserved in the envelope for audit and diagnostics.

## Provenance preservation

Holon's provenance contract:

- Origin, trust, and authority are **never reassigned** by the model.
- When the runtime generates internal messages (system ticks, task results,
  continuation follow-ups), it assigns `System` or `Task` origin with
  `TrustedSystem` trust.
- When an agent delegates work via `SpawnAgent`, the child agent receives
  provenance from the parent's delegation context, not the original operator.
- Event stream consumers (HTTP event stream, TUI) receive provenance labels
  for every message so they can apply their own display/security policies.

## Known gaps

- `TrustLevel` is still present on `MessageEnvelope` alongside the newer
  `AuthorityClass`. The RFC describes this as an intentional transitional
  state with a `From<&TrustLevel> for AuthorityClass` bridge mapping; the
  eventual target is `AuthorityClass`-only. See
  [issue #1385](https://github.com/holon-run/holon/issues/1385).
- `SignedIntegration` is not yet in `AdmissionContext`; the RFC describes
  it as a future direction, so its absence matches current RFC intent.
- `AdmissionContext` is not yet consistently populated across all ingress
  paths; CLI and TUI admissions may carry less metadata than HTTP admissions.
- No standard mechanism exists for integrators to register custom trust
  classification rules for their webhooks or channels.
