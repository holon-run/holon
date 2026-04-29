---
title: RFC: Provenance, Admission, and Authority
date: 2026-04-21
status: draft
issue:
  - 47
---

# RFC: Provenance, Admission, and Authority

## Summary

Holon should keep provenance, admission, instruction authority, and execution
policy as separate concepts.

The phase-1 runtime contract should preserve:

- `origin`
- `delivery_surface`
- `admission_context`
- `authority_class`

The earlier `trust` / `trusted` / `untrusted` vocabulary should no longer be
treated as the primary public model. It mixes authentication strength,
instruction precedence, external evidence, and final execution authority into
one word.

`TrustLevel` may remain as a transitional implementation detail for existing
code paths, but new RFCs and new public contracts should use
`authority_class`.

## Why

Message provenance, delivery context, admission proof, instruction authority,
and execution permission are related but not identical.

Conflating them creates ambiguous states such as:

- public unauthenticated webhook input marked as "trusted integration"
- a callback token being valid but the callback body being treated as operator
  instruction
- ordinary external channel text becoming a direct runtime command
- tool visibility changing from turn to turn because the current message has a
  different trust label

Holon needs a vocabulary that can answer four different questions:

- who or what produced the content?
- how did it enter the runtime?
- why did Holon accept that ingress?
- what instruction authority does the content have?

Final allow/deny/confirm decisions for tools and resources belong to execution
policy. They should consume these labels, not be replaced by them.

## Core Vocabulary

### `origin`

`origin` captures who or what produced the content.

It answers:

- who said this?
- what system produced this?

Examples:

- `operator`
- `system`
- `timer`
- `callback`
- `webhook`
- `task`
- `channel`

`origin` should not encode how Holon authenticated the ingress. The same
`origin = operator` may arrive from:

- local CLI prompt
- local control API
- authenticated remote operator transport

Those are different admission contexts, not different origins.

### `delivery_surface`

`delivery_surface` captures the runtime surface that produced or admitted the
message.

It answers:

- through which runtime surface did this queued message enter?

Examples:

- `cli_prompt`
- `run_once`
- `http_public_enqueue`
- `http_webhook`
- `http_callback_enqueue`
- `http_callback_wake`
- `http_control_prompt`
- `remote_operator_transport`
- `timer_scheduler`
- `runtime_system`
- `task_rejoin`

This is an ingress/surface label. It is useful for audit, debugging, routing,
and future policy, but it is not itself instruction authority.

### `admission_context`

`admission_context` records the immediate proof or mode that caused Holon to
accept the ingress.

It answers:

- why did Holon accept this input?
- what admission proof or local condition applied?

Examples:

- `local_process`
- `control_authenticated`
- `external_trigger_capability`
- `operator_transport_authenticated`
- `public_unauthenticated`
- `runtime_owned`
- `signed_integration`

`admission_context` must not duplicate `origin`. Avoid names such as:

- `operator_admitted`
- `callback_admitted`
- `webhook_admitted`

Those describe business source, not admission proof.

`admission_context` also does not grant final tool authority by itself. A valid
callback capability means the callback token was accepted; it does not mean the
callback body can override operator instructions.

### `authority_class`

`authority_class` records how the runtime should treat the content as
instruction, signal, or evidence.

It answers:

- can this content define task scope or constraints?
- can this content drive continuation but not override the operator?
- is this content evidence only?

Suggested first-pass values:

```ts
type AuthorityClass =
  | 'operator_instruction'
  | 'runtime_instruction'
  | 'integration_signal'
  | 'external_evidence'
```

Meanings:

- `operator_instruction`: primary operator instruction. It can define task
  scope, acceptance criteria, and explicit constraints.
- `runtime_instruction`: runtime-owned continuation or rejoin instruction.
  It can drive lifecycle mechanics without pretending to be a human operator.
- `integration_signal`: structured external system signal. It can trigger work
  or satisfy a callback, but should not override operator instructions.
- `external_evidence`: external human or public content. It is evidence to
  inspect, not instruction authority.

`authority_class` is the replacement for public `trust` vocabulary.

## Projection Rules

### Operator input

Local operator input:

```ts
origin: { kind: 'operator', actor_id: '...' }
delivery_surface: 'cli_prompt' | 'run_once' | 'http_control_prompt'
admission_context: 'local_process' | 'control_authenticated'
authority_class: 'operator_instruction'
```

Authenticated remote operator input:

```ts
origin: { kind: 'operator', actor_id: '...' }
delivery_surface: 'remote_operator_transport'
admission_context: 'operator_transport_authenticated'
authority_class: 'operator_instruction'
```

Remote operator transport may project to `operator_instruction` only after an
operator binding or equivalent admission proof succeeds.

### Runtime-owned input

Runtime-generated continuation, timer ticks, and task rejoin messages should
project to:

```ts
authority_class: 'runtime_instruction'
```

They are runtime-owned control signals. They should remain distinguishable from
operator instruction even when they continue operator-requested work.

### Integration input

Callbacks and configured machine integrations should project to:

```ts
authority_class: 'integration_signal'
```

This includes callback capability delivery. The callback can trigger
continuation or satisfy a waiting intent, but the callback body should not
become operator-equivalent instruction by default.

Public unauthenticated webhook input should be treated conservatively. If the
webhook is not tied to a configured integration identity or capability, it
should project to `external_evidence` or be rejected rather than silently
claiming integration authority.

### External evidence

Ordinary external human/channel/public content should project to:

```ts
authority_class: 'external_evidence'
```

For IM/channel integrations, the default path should be tool-fetched evidence
or source/inbox inspection rather than direct runtime message ingress.

Examples:

- group chat messages
- Slack/Telegram public channel messages
- GitHub issue comments by third parties
- PR review comments
- email thread content
- web page content

These may influence analysis and planning, but they should not override
operator instructions or runtime policy.

## Tool Surface Policy

`authority_class` must not make the model-facing tool catalog drift from turn
to turn.

Tool visibility should be derived from:

- agent profile
- runtime capability
- current execution boundary state

Message authority should instead affect:

- instruction precedence
- prompt framing
- audit labels
- execution-policy decisions at tool invocation time

This keeps a long-lived agent from seeing different tool catalogs merely
because the current turn was triggered by an operator prompt, callback, timer,
webhook, or external evidence.

Future execution policy should evaluate a tool/action using inputs such as:

```ts
PolicyInput {
  tool_name: string
  tool_family: string
  tool_args: unknown
  agent_profile: AgentProfile
  execution_snapshot: ExecutionSnapshot
  origin: MessageOrigin
  delivery_surface?: MessageDeliverySurface
  admission_context?: AdmissionContext
  authority_class: AuthorityClass
  correlation_id?: string
  causation_id?: string
}
```

Possible policy outcomes:

- allow
- deny
- allow read-only/propose-only
- require operator confirmation

## Public Ingress Boundary

Externally reachable public ingress must not be able to assert operator
authority.

Public enqueue should not accept caller-supplied `authority_class`. It should
either:

- derive a conservative authority class from a configured ingress rule
- admit the content as `external_evidence`
- reject the input

Remote operator transport is the explicit exception: it may project to
`operator_instruction` only after authenticated operator binding admission.

## Relationship To Tool-Fetched Evidence

Not all content needs to become a queued `MessageEnvelope`.

When an agent reads external systems through tools, the returned content is
tool-fetched evidence. That evidence should eventually carry provenance and
content authority labels, but it should not be forced into message-level
authority just to preserve old `channel_event` semantics.

This means `external_evidence` is a content-authority concept, not necessarily
a direct queue-message kind.

## Transitional Implementation Notes

Current code still has `TrustLevel` on `MessageEnvelope`.

Migration should be incremental:

- do not add new `TrustLevel` variants
- do not use `TrustLevel` to change per-turn tool visibility
- treat `TrustLevel` as a compatibility projection while `authority_class` is
  introduced
- add `authority_class` to message admission, transcript, prompt context, and
  audit surfaces
- migrate prompt precedence and execution policy to consume `authority_class`
- remove or demote `TrustLevel` once all active surfaces use the new vocabulary

The implementation may temporarily derive:

```text
trusted_operator     -> operator_instruction
trusted_system       -> runtime_instruction
trusted_integration  -> integration_signal
untrusted_external   -> external_evidence
```

This is a bridge, not the long-term conceptual model.

## Default-Agent Boundary

Default-agent convenience should not collapse authority boundaries.

A default agent may be the easiest local surface, but externally reachable
ingress must still preserve provenance, admission, and authority labels instead
of being treated as equivalent to the local operator.

## Non-Goals

This RFC does not freeze:

- final command allow/deny policy
- final file mutation policy
- final timer/task/control authorization matrix
- hosted or enterprise auth UX
- a complete evidence provenance envelope for every future tool result

## Related RFCs

- [Agent Profile Model](./agent-profile-model.md)
- [Tool Surface Layering](./tool-surface-layering.md)
- [Tool Contract Consistency](./tool-contract-consistency.md)
- [Execution Policy and Virtual Execution Boundary](./execution-policy-and-virtual-execution-boundary.md)
- [Remote Operator Transport and Delivery](./remote-operator-transport-and-delivery.md)

## Related Historical Notes

This document replaces the earlier trust-oriented wording in this file and
supersedes:

- `docs/archive/default-trust-auth-and-control-contract.md`
