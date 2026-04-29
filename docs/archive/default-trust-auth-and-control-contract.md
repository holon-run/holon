# Default Trust, Auth, And Control Contract

Date: 2026-04-08

Related issue:

- `holon-run/holon#47` `Freeze default trust, auth, and control contract`

Related RFCs:

- [docs/result-closure-contract.md](result-closure-contract.md)
- [docs/continuation-trigger-contract.md](continuation-trigger-contract.md)
- [docs/objective-delta-and-acceptance-boundary.md](objective-delta-and-acceptance-boundary.md)
- [docs/agent-types-and-default-agent.md](agent-types-and-default-agent.md)

This RFC defines the default provenance and authority-marking contract for
runtime inputs and control surfaces in `Holon`.

The core question is:

- how should `Holon` mark where an input came from, what authority class it
  belongs to, and what runtime surface admitted it?

This RFC intentionally does not freeze the final restriction policy. That later
policy should be designed together with execution policy, resource authority,
and sandbox behavior.

## Problem

`Holon` already has pieces of a trust model:

- origin-aware messages
- trust levels
- HTTP auth gates
- control-token protection
- callback, timer, task, and operator ingress paths

But those pieces are still mixed together with policy assumptions.

This makes it too easy to blur three different questions:

1. where did this input come from?
2. what authority class should the runtime assign to it?
3. what concrete restrictions should apply to resources and execution?

The first two should be defined now.
The third should be deferred until `Holon` has a clearer execution-policy and
resource model.

## Goal

This RFC answers four questions:

1. What provenance fields should every input carry?
2. What default trust vocabulary should `Holon` use?
3. How should authentication and runtime authority relate?
4. What should remain intentionally undecided until execution/resource policy
   work is ready?

## Non-Goals

This RFC does not define:

- the final execution-policy model
- the final sandbox backend or enforcement strategy
- the final resource-authorization matrix
- per-tool allow/deny rules
- whether a given task shape is allowed for a given trust level

In particular, it does not try to decide things like:

- whether channel-originated input may create some specific task type
- whether a given trust level may read or write a given workspace
- whether a callback may mutate a specific resource

Those decisions should later be made in terms of resource authority and
execution policy, not only in terms of message trust.

## Core Judgments

### 1. Provenance and restriction policy should be separated

`Holon` should first define a stable marking model:

- where the input came from
- how it entered the runtime
- what trust class it belongs to
- which agent it targets

It should not prematurely freeze the final restriction policy on top of those
marks.

### 2. Origin and trust are related, but not identical

Every input should have:

- an origin
- a trust level

Origin answers:

- where the message came from

Trust answers:

- what authority class the runtime should assign to it by default

The runtime should keep both concepts because:

- different origins may share one trust level
- the same origin class may later be mapped differently by configuration or
  deployment policy

### 3. Authentication and runtime authority should stay distinct

Authentication answers:

- may this caller access this transport or control surface?

Runtime authority answers:

- once the input is admitted, how should it be marked inside the runtime?

So:

- auth success should not automatically imply operator authority
- bearer-token access to an HTTP surface should not by itself rewrite the
  message's runtime trust class

### 4. Resource authority matters more than task shape

The later restriction policy should be defined mainly in terms of resource
access, not only in terms of task names or high-level workflow labels.

The more important future questions are:

- what files may be read or written?
- what workspace or worktree may be touched?
- what agent state may be mutated?
- what callback or control surfaces may be exercised?
- what network or secret-bearing resources may be accessed?

Those questions belong to execution/resource policy, not to this RFC.

## Provenance Model

Every runtime input should carry at least:

- `origin`
- `trust`
- `target_agent_id`
- `admission_context`
- `correlation_id`
- `causation_id`

Message-producing ingress should additionally carry:

- `delivery_surface`

Meaning:

- `origin`
  - where the message came from in product terms
- `trust`
  - the default authority class assigned by the runtime
- `target_agent_id`
  - which agent the message is routed to
- `admission_context`
  - the admission posture used to accept the input
- `correlation_id`
  - logical request grouping
- `causation_id`
  - what prior action or event caused this message
- `delivery_surface`
  - which message-producing ingress admitted the queued message

Phase 1 does not require all of these to be exposed equally everywhere, but
they form the right contract.

## Origin Vocabulary

The default origin vocabulary remains:

- `operator`
- `system`
- `task`
- `timer`
- `webhook`
- `callback`
- `channel`

This matches the current runtime shape and should remain the stable public
baseline.

## Trust Vocabulary

The default trust vocabulary remains:

- `trusted_operator`
- `trusted_system`
- `trusted_integration`
- `untrusted_external`

These should remain the runtime's default authority classes.

## Default Mapping By Origin

The baseline default mapping should be:

- `operator` -> `trusted_operator`
- `system` -> `trusted_system`
- `task` -> `trusted_system`
- `timer` -> `trusted_system`
- `webhook` -> `trusted_integration`
- `callback` -> `trusted_integration`
- `channel` -> `untrusted_external`

This is a marking default, not the final restriction policy.

It answers:

- how the runtime classifies the input by default

It does not by itself answer:

- what resources the input may access

## Delivery Surface

`Holon` should explicitly track which runtime surface admitted a queued
runtime message.

Examples:

- CLI operator prompt
- HTTP message ingress
- HTTP control surface
- callback endpoint
- webhook endpoint
- timer scheduler
- runtime-owned system emission
- task or child-agent rejoin

This matters because two inputs may share the same trust class while still
arriving through different transport and auth paths.

Pure control-plane mutations such as:

- workspace attach / use
- create timer
- create task
- create named agent

do not need to become queued messages just to preserve provenance. For those
operations, phase 1 should preserve provenance in audit events instead.

## Admission Context

The runtime should preserve a compact admission posture for ingress, such as:

- public unauthenticated
- control authenticated
- callback capability
- local process
- runtime owned

This is not the same thing as trust.

It is provenance about admission, not a replacement for runtime authority
classification.

## Authority Intent Marking

Even before final restriction policy exists, `Holon` should mark what kind of
authority the runtime should be careful not to assume.

The most important default rule is:

- external continuation authority must not silently be treated as operator
  authority

That means the runtime should be able to preserve a distinction between:

- operator-originated intent
- integration-originated continuation
- external low-trust content
- runtime-owned system behavior

This is still a marking rule, not yet a full enforcement matrix.

## Default Agent And Authority

The `default agent` should not be treated as privileged by name alone.

Agent kind affects:

- routing
- visibility
- durability

It should not by itself affect provenance or trust classification.

So:

- a message to the default agent still needs origin and trust marks
- a message to a named or child agent follows the same provenance contract

## What This RFC Freezes

This RFC freezes:

- the provenance vocabulary
- the trust vocabulary
- the default origin-to-trust mapping
- the rule that authentication and runtime authority are distinct
- the rule that restriction policy should later be defined around resource
  authority

## What This RFC Intentionally Leaves Open

This RFC intentionally leaves open:

- which trust levels may create which task shapes
- which trust levels may read or write which files
- which trust levels may touch which execution roots
- which trust levels may mutate objective or control state
- the final sandbox enforcement strategy

Those should be decided later together with:

- execution policy
- resource authority model
- sandbox design

## Phase 1 Direction

Phase 1 should aim to make provenance explicit and inspectable:

- preserve origin and trust consistently
- preserve delivery-surface on queued runtime messages
- preserve admission context on both queued messages and control-plane audit
- align docs and runtime defaults on these marks
- avoid implicit trust upgrades caused by transport choice

This is enough to prepare the next layer of policy work without freezing the
wrong constraints too early.

## Invariants

1. Every admitted input should have stable origin and trust marks.
2. Authentication should not silently imply operator authority.
3. Origin and trust should remain separate concepts.
4. The `default agent` should not be privileged by name alone.
5. Later restriction policy should be expressed mainly in terms of resource
   authority and execution policy, not only task names.

## Decision

`Holon` should freeze a default provenance and authority-marking contract, not
yet a full restriction matrix. The runtime should preserve origin, trust,
delivery surface, and admission context as separate facts, and future
restriction policy should be designed primarily around resource authority,
execution policy, and sandbox behavior.
