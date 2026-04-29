---
title: RFC: Agent Profile Model
date: 2026-04-22
status: draft
---

# RFC: Agent Profile Model

## Summary

This RFC defines Holon's first-pass agent profile model.

The central direction is:

- agent profile is a stable capability package
- profile is expressed through a small preset enum, not a free-form tool or
  policy object in `SpawnAgent`
- profile controls stable capability families, not interaction role and not
  fine-grained execution-policy boundaries
- profile presets should be few, operator-readable, and easy for the model to
  choose

This RFC builds on:

- `tool-surface-layering.md`
- `tool-contract-consistency.md`
- `agent-control-plane-model.md`
- `agent-delegation-tool-plane.md`

## Problem

Holon now has the right ingredients for a profile model, but no single contract
yet defines how they fit together.

Today we already have:

- agent identity and lifecycle distinctions such as `visibility`
- stable tool-family work in the tools RFCs
- `SpawnAgent` as the agent-plane creation primitive
- a desire to stop changing tool visibility based on per-message trust labels

Without an explicit profile model, several problems appear quickly:

- `SpawnAgent` risks growing into a free-form bag of capability parameters
- tool-family availability has no stable home at the agent level
- visibility and supervision remain disconnected from capability packaging
- future execution-policy work has no clean boundary against agent capability
  defaults

Holon should solve this now with a small profile model rather than waiting until
many partially-overlapping spawn parameters already exist.

## Goals

- define agent profile as a stable capability package
- keep `SpawnAgent` profile selection simple through a small preset enum
- define the relationship between profile and tool families
- define the relationship between profile and supervision contract
- separate profile from future execution-policy and environment-boundary work
- establish first-pass preset profiles for the default agent and child agents

## Non-goals

- do not define interaction mode or agent persona in this RFC
- do not define `AGENTS.md` behavior roles in this RFC
- do not define resource-boundary policy such as path allowlists in this RFC
- do not expose free-form per-tool toggles in the public profile model
- do not require immediate support for arbitrary custom profiles

## Core Judgment

Agent profile should answer:

- what stable capability families does this agent have by default?
- is this agent public or private?
- is this agent parent-supervised or self-owned?

Agent profile should not answer:

- what persona or workflow style the agent should follow
- which exact filesystem roots or execution targets are allowed
- which exact prompt addendum applies to a later named role

In short:

- profile defines stable capability families
- execution policy defines resource and boundary constraints
- runtime state defines temporary availability

## Profile Config Shape

The internal profile configuration should be expressed as:

```ts
type AgentProfileConfig = {
  identity: {
    visibility: 'public' | 'private'
  }
  lifecycle: {
    ownership: 'parent_supervised' | 'self_owned'
  }
  tool_families: {
    core: boolean
    local_environment: boolean
    agent_creation: boolean
    authority_expansion: boolean
    external_trigger: boolean
  }
}
```

This is the internal mapping target for preset profile names.

The public spawn surface should not require the model to submit this whole
object directly in the first pass.

## Identity Fields

The profile model should continue to use Holon's existing identity dimensions:

- `visibility`

### Visibility

`visibility` means whether the agent belongs to the public operator-visible
agent surface.

- `public` means the agent is part of the normal public agent listing and is a
  first-class operator-facing agent object
- `private` means the agent is hidden from normal public listings and is
  primarily an internal runtime context

In the first-pass preset set, the default private child profile also implies:

- the agent belongs to a parent-owned supervision tree
- `SpawnAgent` should return a `task_handle` for that child
- parent cleanup should recursively clean up private descendants

`visibility` does not by itself define:

- tool visibility
- execution authority
- lifecycle ownership
- external trigger capability

### Ownership

`ownership` means who owns the agent's operational lifecycle.

- `parent_supervised` means the agent remains inside a parent-owned supervision
  contract
- `self_owned` means the agent is an operator-facing runtime object with its
  own independent lifecycle surface

`ownership` does not by itself define:

- tool visibility
- execution authority
- external trigger capability

What it does define is:

- whether `SpawnAgent` should return a `task_handle`
- who owns cleanup responsibility
- whether the agent must remain restart-safe while that supervision handle is
  still live

## Tool Families

Profiles should enable or disable stable capability families rather than
individual tool names.

The current family set is:

- `core`
- `local_environment`
- `agent_creation`
- `authority_expansion`
- `external_trigger`

This keeps profile stable even if individual tool names evolve.

### Mapping Rule

The model-facing tool surface for one agent should be derived from:

1. `profile.tool_families`
2. runtime capability
3. current execution state

That means:

- profile says whether a family is part of the agent's stable capability package
- runtime capability says whether the runtime can honor that family at all
- current state says whether a tool in that family is temporarily available now

This keeps profile, execution policy, and transient state from collapsing into
one mechanism.

## Relationship To Execution Policy

Profile should not try to encode fine-grained boundary rules.

For example:

- whether `AttachWorkspace` may attach any path or only a preauthorized root
- whether managed worktrees are allowed in this runtime
- whether a given workspace projection is currently legal

These belong to execution policy or execution environment design, not to the
first-pass profile object.

The intended split is:

- profile decides whether the agent has a capability family at all
- execution policy decides the resource boundary when that family is used

## Relationship To Spawn Result Contract

The first-pass spawn result rule should be simple:

- `SpawnAgent` always returns `agent_id`
- private spawned agents must also return `task_handle`
- public named agents should return `agent_id` without `task_handle`

The reason is lifecycle ownership.

For the first pass:

- `private_child` agents are parent-supervised by definition
- `public_named` agents are self-owned operator-facing runtime objects by
  definition

This keeps the spawn contract easy to understand and avoids introducing a
separate supervision dimension before it is needed.

## Public Preset Names

The first-pass public surface should use a small preset enum rather than
allowing free-form composition in `SpawnAgent`.

The initial preset set should be:

- `private_child`
- `public_named`

These names are shortcuts. They map onto the internal profile configuration
shape above.

## First-Pass Presets

## 1. `private_child`

`private_child` is the default bounded child-agent profile.

It maps to:

```ts
{
  identity: {
    visibility: 'private',
  },
  lifecycle: {
    ownership: 'parent_supervised',
  },
  tool_families: {
    core: true,
    local_environment: true,
    agent_creation: false,
    authority_expansion: false,
    external_trigger: true,
  }
}
```

This means:

- private child agents are hidden from the normal public listing surface
- they remain inside a parent-owned supervision contract
- they may operate inside the existing local execution boundary
- they may create external trigger capabilities when needed
- they may not create further child agents by default
- they may not expand authority boundaries such as attaching a new workspace by
  default
- `SpawnAgent` should return a `task_handle` for these children
- while that `task_handle` is live, the child must remain restart-safe
- normal terminal cleanup is driven by the supervising task contract, not by a
  separate lifetime flag
- if the parent is deleted, archived, or explicitly cleaned up, the runtime
  should recursively clean up these private descendants

This profile should be the default for `SpawnAgent`.

## 2. `public_named`

`public_named` is the first-pass public self-owned profile.

It maps to:

```ts
{
  identity: {
    visibility: 'public',
  },
  lifecycle: {
    ownership: 'self_owned',
  },
  tool_families: {
    core: true,
    local_environment: true,
    agent_creation: true,
    authority_expansion: true,
    external_trigger: true,
  }
}
```

This means:

- the agent is public and operator-visible
- the agent has its own lifecycle surface rather than a parent-scoped
  supervision handle
- the agent has the full first-pass capability family set
- `SpawnAgent` should return `agent_id` without `task_handle` when this profile
  is created as a public named agent

For the first pass, `public_named` is intentionally broad rather than split into
multiple public presets.

## Default Mapping

The intended defaults are:

- default/root agent -> `public_named`
- `SpawnAgent` default -> `private_child`

This gives Holon a simple first-pass rule:

- top-level named agents are public and self-owned
- delegated child agents are private and parent-supervised by default

## Why `private_child` Includes External Trigger Capability

`private` should not be treated as "cannot use external trigger capabilities."

A private child may still need to:

- wait on a callback
- resume after an external event
- re-enter through a machine-facing wake channel

So the first-pass model keeps these separate:

- `visibility` controls public discoverability
- `external_trigger` controls whether the agent may use waiting-plane external
  trigger tools

This separation is important and should remain explicit.

What stays intentionally coupled in the first pass is different:

- `private_child` stays coupled to parent supervision and `task_handle`
- `external_trigger` remains independent from that coupling

## Why There Are Only Two Presets In The First Pass

Holon should begin with very few profile choices.

The main reason is not lack of imagination. The reason is interface clarity.

Too many early profile variants would:

- make `SpawnAgent` harder for the model to choose from
- force premature distinctions that may really belong to later execution-policy
  or interaction-mode work
- make the first public contract harder to explain

So the first pass should prefer:

- one default bounded child preset
- one default public self-owned preset

Additional presets can be added later if real use cases justify them.

## Deferred Questions

This RFC intentionally defers:

- whether Holon later needs more preset profiles such as a child-delegating
  private preset
- whether a later model should separate ownership more explicitly from
  preset naming
- whether public self-owned agents should later split into narrower capability
  packages
- whether execution policy should be bound directly to profiles or layered
  separately
- whether custom operator-defined profiles should exist in a later version
- whether profile inheritance or profile aliases should exist

## Summary

Holon should adopt a small agent profile model now.

The first-pass model should:

- define `visibility` and lifecycle ownership separately
- define stable capability families through `tool_families`
- keep fine-grained resource boundaries out of the profile object
- expose only a small preset enum in `SpawnAgent`

The initial presets should be:

- `private_child`
- `public_named`

This gives Holon a clear and stable foundation for agent capability packaging
without overcomplicating the first public contract.
