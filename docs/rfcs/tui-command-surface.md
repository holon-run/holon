---
title: RFC: TUI Command Surface
date: 2026-04-22
status: draft
---

# RFC: TUI Command Surface

## Summary

This RFC proposes a dedicated command surface for the Holon TUI.

The central direction is:

- use `/` as the TUI command prefix
- treat the command surface as a thin operator-facing UI control layer
- keep command semantics separate from normal agent chat input
- avoid exposing high-risk runtime mutations through direct TUI commands
- converge low-frequency action-menu operations into one command surface over
  time

The TUI command surface should make operator actions more discoverable without
turning the composer into a second shell or a second runtime control CLI.

In the first version, the command surface should remain explicitly narrow:

- use `/` rather than `:`
- focus on local UI control and read-oriented inspection
- reject direct operator-driven execution-root or worktree mutation
- converge safe action-menu behavior into slash commands over time

## Problem

The current TUI has three different kinds of interaction mixed together:

- normal operator chat input to the selected agent
- keyboard shortcuts such as `Ctrl+A`, `Ctrl+E`, `Ctrl+J`
- a small `:`-triggered action menu

This creates several problems.

First, low-frequency actions are not discoverable enough.

Shortcuts are fast for repeat users, but they are hard to learn and easy to
forget for operations that happen infrequently.

Second, the current `:` behavior is not actually a command system.

Today `:` only opens an action menu when the composer is empty. It does not
parse commands, does not support arguments, and does not create a coherent
operator-facing control surface.

Third, the action menu currently contains operations that are too powerful for
direct operator override.

In particular, workspace/worktree switching currently lets an external surface
mutate an agent's active execution projection. That is a runtime-boundary
problem, not just a TUI polish problem.

Finally, without a deliberate command model, the TUI risks drifting into an
unclear hybrid:

- part chat client
- part keybinding-heavy control panel
- part runtime admin console

That would make the product harder to reason about and harder to teach.

## Goals

- define one explicit command prefix for TUI commands
- separate operator chat input from TUI control input
- make low-frequency TUI actions discoverable and composable
- keep the first version intentionally narrow
- align the TUI command surface with Holon's runtime trust boundaries
- prepare a path to replace the current `:` action menu
- define a small first-phase command set that is implementable without
  expanding runtime authority

## Non-goals

- do not create a shell-like command language inside the TUI
- do not expose arbitrary runtime internals through TUI commands
- do not support direct operator mutation of an active agent's execution
  projection
- do not replace the main `holon` CLI with the TUI command surface
- do not introduce scripting, aliases, or complex pipelines in the first
  version

## Core Judgment

The TUI should support slash commands, but only as a thin UI control layer.

That means:

- `/...` controls the TUI or submits operator intent
- normal text continues to be sent to the selected agent
- slash commands must not silently act as a privileged runtime backdoor

The TUI is a conversational surface first. Its command system should assist
navigation, inspection, and explicit operator intent, not become a second
operator control plane with broader authority than the chat surface itself.

This implies a strict design rule:

- if a control can unexpectedly rewrite an active agent's execution context, it
  should not be exposed as a direct slash command

## Prefix Choice

The TUI should standardize on `/` as the command prefix.

Reasons:

- `/` matches common expectations from chat-first interfaces
- `/` feels like a command palette inside a conversation surface
- `:` carries stronger editor-command and shell-command associations
- `/` better fits Holon's operator-chat mental model

The current `:` action-menu trigger should be treated as transitional.

The recommended migration path is:

1. add `/` command parsing
2. update TUI help and prompts to teach `/`
3. keep `:` as a temporary compatibility trigger if needed
4. later remove the `:` action menu entry point

## Command Parsing Model

Slash commands should use a simple first-token parser.

The rules should be:

- a command is only recognized when the first character of the draft is `/`
- commands are parsed from the first line of the draft
- unrecognized commands must fail explicitly in the status surface
- slash-prefixed normal text must have an escape path

One acceptable escape rule is:

- `//hello` sends `/hello` as normal chat text

The parser should stay intentionally simple in the first version:

- command name
- space-separated arguments
- no nested quoting model beyond what is required for minimal path support

If later requirements grow, richer parsing can be added after the command
surface proves useful.

## Command Surface Boundary

The TUI command surface should only expose actions that are appropriate for
direct operator invocation.

That means the command surface is a good fit for:

- opening overlays
- refreshing the current projection
- clearing local UI state
- expressing explicit operator intent in a structured way

It is not a good fit for:

- arbitrary runtime mutation
- privileged context rewrites
- hidden lifecycle overrides
- direct execution-root or worktree switching for an active agent

This boundary matters because the TUI should not silently bypass the runtime's
trust and context model.

## Disallowed Direct Commands

The following direct command categories should not be supported in the TUI
command surface:

### 1. Direct workspace/worktree projection switching

Commands such as these should not exist as direct TUI controls:

- `/root`
- `/worktree <branch>`
- `/enter-workspace ...`

Those transitions rewrite the active agent's execution context and can confuse
an in-flight agent if done externally.

If such a transition is needed, it should be:

- initiated by the agent itself
- or handled by runtime-owned lifecycle logic with explicit safety rules

### 2. Arbitrary shell or runtime admin commands

Commands such as these should not exist:

- `/exec ...`
- `/run ...`
- `/control ...`

Those belong either to the agent tool plane or to the external CLI, not the
chat-first TUI surface.

## First-Phase Command Set

The first version should stay narrow and operator-safe.

Recommended initial commands:

- `/help`
- `/agents`
- `/events`
- `/tasks`
- `/transcript`
- `/refresh`
- `/clear-status`

These commands are all local-UI or read-oriented control actions.

They do not rewrite agent execution context and they match capabilities the TUI
already exposes through shortcuts or overlays.

The first phase should not add command arguments unless they are required for a
clear local-UI action.

## Suggested Command Semantics

## 1. `/help`

Shows the TUI command list and key behavior.

Its job is:

- discoverability
- syntax reminder
- migration aid from the current `:` action menu

## 2. `/agents`

Opens the agents overlay.

This is the command form of `Ctrl+A`.

## 3. `/events`

Opens the raw events overlay.

This is the command form of `Ctrl+E`.

## 4. `/tasks`

Opens the tasks overlay.

This is the command form of `Ctrl+J`.

## 5. `/transcript`

Opens the transcript overlay.

This is the command form of `Ctrl+T`.

## 6. `/refresh`

Requests a fresh `/state` bootstrap for the current agent.

This is especially useful when:

- the projection is marked stale
- the operator wants to force a state resync
- the event stream has recovered from a temporary inconsistency

This is a UI/runtime resync command, not a semantic agent instruction.

## 7. `/clear-status`

Clears transient local status text in the TUI.

This should affect only the TUI's local status presentation, not runtime state.

## Operator Intent Commands

There is one additional category worth leaving open for later:

- commands that translate into explicit operator intent messages

Examples could include:

- `/request <structured operator intent>`
- `/note <structured operator note>`

These are different from direct runtime mutation.

They do not change the agent's context behind its back. Instead, they produce
explicit operator-authored input that the agent can interpret and act on.

This category is valid, but it should only be added after the basic local UI
command set is stable.

## Relationship To The Existing Action Menu

The current action menu should be treated as transitional.

Its current items fall into two buckets:

### Safe candidates for command migration

- debug/help/navigation style actions

### Unsafe candidates that should not become direct slash commands

- canonical-root entry
- git-worktree entry

The action menu should therefore not be copied wholesale into `/` commands.

Instead, the command surface should become the new primary operator-facing
control layer, while unsafe direct actions are removed rather than migrated.

This means command adoption is also a cleanup effort.

The goal is not to preserve every current action-menu entry in a new syntax.
The goal is to narrow the operator-facing control surface to actions that
respect runtime boundaries.

## Error Handling

The command surface should fail explicitly and locally.

Recommended rules:

- unknown command -> status error with `/help` hint
- invalid arguments -> status error with expected usage
- unsupported command in current state -> status error explaining the state

Command errors should not silently fall through into normal chat sending.

That would create ambiguous behavior and make operators distrust the surface.

## Interaction With The Composer

The composer should remain the main chat surface.

Slash commands therefore need a clear interaction model:

- when the first character is `/`, treat the draft as a command draft
- pressing `Enter` executes the command
- `Shift+Enter` should not be used to build multi-line slash command scripts in
  the first version
- when the draft does not begin with `/`, normal chat send behavior remains
  unchanged

This keeps the mental model simple:

- chat by default
- command only when explicitly prefixed

The composer should not become a mini shell:

- multi-line chat remains valid
- slash commands remain single-shot control entries
- command execution should produce local status or overlay effects, not a new
  command transcript language

## Relationship To Runtime Trust Boundaries

This RFC intentionally aligns the TUI command surface with Holon's trust model.

The command surface should not give the operator an implicit privileged path
that is stronger than the user-facing conversation contract.

In particular:

- external operator input can request actions
- external operator input can inspect state through permitted UI surfaces
- external operator input should not directly rewrite an active agent's
  execution context from the TUI

This principle is especially important for workspace/worktree behavior.

## Migration Plan

## Phase 1: Introduce Slash Parsing

- add `/` command detection and dispatch
- implement the minimal safe command set
- teach `/help`
- preserve normal chat send behavior for non-command drafts

## Phase 2: Update TUI Guidance

- update status/help/composer hints to use `/`
- stop teaching `:` as the main operator entry point

## Phase 3: Remove Transitional Action Menu Paths

- remove direct action-menu dependence for commands that now have `/`
  equivalents
- remove unsafe direct workspace/worktree actions instead of migrating them

## Adoption Model

This RFC should be adopted in two tracks.

The first track is product-shape cleanup:

- establish `/` as the canonical command prefix
- stop expanding `:` into a competing command model
- narrow the operator-facing command set to safe local actions

The second track is implementation cleanup:

- route existing safe overlay actions through one parser/dispatcher
- keep command failures local and explicit
- delete unsafe direct workspace/worktree operator controls instead of
  rebranding them

## Open Questions

### 1. Should `:` remain as a compatibility alias temporarily?

Probable answer:

- yes for a short transition window
- no as a long-term primary command entry point

### 2. Should `/refresh` be synchronous or fire-and-return?

Probable answer:

- fire-and-return with visible status updates

### 3. Should operator-intent slash commands exist later?

Probable answer:

- yes, but only when they produce explicit operator messages rather than hidden
  runtime mutation

## Decision

Holon should add a slash-command surface to the TUI, but keep it narrow.

The TUI command surface should:

- use `/`
- focus on safe UI and inspection actions
- avoid direct runtime context mutation
- replace the current `:` action menu over time rather than coexist forever

This gives the TUI a clearer control model while preserving runtime clarity and
trust boundaries.
