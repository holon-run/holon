---
title: RFC: Runtime Configuration Surface
date: 2026-04-22
status: draft
issue:
  - 370
---

# RFC: Runtime Configuration Surface

## Summary

Holon should separate configuration by lifecycle and ownership.

The runtime should use three distinct layers:

- startup settings
- runtime-mutable configuration
- agent state

The core contract is:

- startup settings are provided only through environment variables and CLI
  flags
- `config.json` stores only runtime-mutable configuration
- agent-specific overrides and behavior state belong to agent state, not to
  `config.json`

## Why

Configuration is currently too easy to interpret as one flat mutable surface.
That creates ambiguity:

- some values are only meaningful during process startup
- some values should affect future turns of a running runtime
- some values belong to one specific agent rather than the runtime as a whole

Holon should make these lifecycles explicit instead of relying on operator
guesswork.

## Lifecycle Layers

## 1. Startup Settings

Startup settings define how the process is bootstrapped.

They include values such as:

- home and data roots
- listener and socket binding
- control-plane bootstrap and authentication inputs
- other host wiring values that are only interpreted during startup

Startup settings are not part of Holon's runtime mutation surface.

### Contract

- startup settings are supplied through environment variables and CLI flags
- startup settings are read during process startup
- Holon does not provide a config-mutation command surface for them
- changing startup settings requires restarting the affected process

## 2. Runtime-Mutable Configuration

Runtime-mutable configuration controls behavior that a running Holon host may
change without redefining process wiring.

Examples include:

- default model selection
- model fallback chain
- model metadata overrides keyed by `provider/model`
- explicit unknown-model fallback policy
- other future runtime behavior settings that naturally apply to future turns

### Contract

- runtime-mutable configuration is the only configuration class stored in
  `config.json`
- runtime-mutable configuration is the only configuration class exposed through
  runtime configuration mutation surfaces
- mutating runtime configuration affects future turns, not in-flight provider
  turns
- a running host should be able to report the currently effective runtime
  configuration

For model configuration specifically:

- built-in model metadata may be compiled into Holon from local source mirrors
- `config.json` may override runtime policy fields for known `provider/model`
  entries
- `config.json` may define one explicit fallback policy for unknown models
- the runtime should expose both the effective model ref and the resolved
  runtime policy actually in use

## 3. Agent State

Agent state is neither startup configuration nor runtime-wide configuration.

It includes agent-specific persisted values such as:

- model override
- workspace attachment state
- waiting and work state
- future agent-scoped bootstrap or role state

### Contract

- agent state is persisted under the agent's own storage
- agent state mutation uses agent control surfaces, not runtime config
- agent state may override runtime-wide defaults where the agent contract
  explicitly allows it

## `config.json`

`config.json` remains the persisted file-backed store for runtime-mutable
configuration.

It should not be used as a mixed store for:

- startup-only process wiring
- per-agent state
- operator-facing ephemeral session state

This keeps the file's meaning stable:

- it is the persisted source of truth for runtime behavior settings that a
  running Holon runtime may also expose through control APIs

## Mutation Surface

Holon should keep mutation surfaces aligned with lifecycle class:

- startup settings: env vars and CLI flags only
- runtime-mutable config: runtime control API and matching CLI surface
- agent state: agent control API and matching CLI surface

These surfaces should not be collapsed into one generic `config` abstraction.

## Status And Inspectability

Holon should make the active source of truth inspectable:

- which startup settings were used to boot the process
- which runtime configuration is currently effective
- which agent-specific overrides are active for one agent

The operator should not have to infer lifecycle class from file location or
command naming alone.

For model-aware runtime configuration, inspectability should include:

- the effective model ref
- the resolved prompt/compaction/output policy derived for that model
- whether the resolved policy came from a built-in catalog entry, a config
  override, or an explicit unknown-model fallback

## Non-Goals

- do not define every startup flag in this RFC
- do not define the final CLI syntax for runtime-config mutation
- do not redesign agent profile or instruction-loading behavior here
- do not require startup settings to be persisted in a second config file

## Related RFCs

- `agent-control-plane-model.md`
- `agent-profile-model.md`
- `instruction-loading.md`
- `execution-policy-and-virtual-execution-boundary.md`
