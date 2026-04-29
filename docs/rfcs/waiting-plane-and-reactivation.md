---
title: RFC: Waiting Plane And Reactivation
date: 2026-04-21
status: draft
---

# RFC: Waiting Plane And Reactivation

## Summary

This RFC proposes that Holon should treat waiting as its own public tool plane,
rather than leaving it split across task language, callback language, and
sleep/timer language.

The central direction is:

- waiting is a first-class runtime concern
- waiting should be expressed through explicit waiting-oriented tools
- callback, timer, and wake-only flows should converge under one waiting plane

Within that plane, callback-backed ingress should be treated as its own public
sub-family:

- external trigger tools

This RFC does not redefine Holon's continuation trigger contract. Instead, it
defines where the public tools for waiting should live and what semantics they
should own.

## Problem

Holon already has several concepts related to future resumption:

- `Sleep`
- `CreateExternalTrigger`
- `CancelExternalTrigger`
- timers
- wake hints
- waiting intents

Each piece is individually reasonable, but the public surface still feels
fragmented.

Today waiting appears partly as:

- model intent to stop now
- callback capability creation
- timer scheduling
- background work language

This creates several problems:

- waiting semantics are not grouped together in one public mental model
- timer-backed wake and callback-backed wake can feel like unrelated features
- `sleep_job` risks becoming a generic fallback for things that are not really
  task execution
- prompt guidance has to explain too many adjacent concepts separately

## Goals

- define a coherent public waiting plane
- make callback-backed and timer-backed waiting feel like one family
- keep waiting distinct from command execution and agent delegation
- give work items a cleaner relationship to blocked and resumable state

## Non-goals

- do not replace the continuation trigger contract
- do not redefine event-stream payloads
- do not require one single waiting tool to cover every possible future case
- do not eliminate Holon's distinction between wake-only and contentful
  continuation

## Core Judgment

Waiting is not command execution.

Waiting is not delegation.

Waiting is not high-level work identity.

Waiting is the public/runtime layer that answers:

- what is the agent blocked on?
- what future condition should wake it?
- how can that wait be cancelled or replaced?

Holon should therefore treat waiting as a first-class plane, not as leftover
behavior attached to whichever other tool happens to be involved.

## Waiting Plane Responsibilities

The waiting plane should own:

- creation of durable waiting intents
- timer-backed reactivation
- callback-backed reactivation
- external wake and re-entry channels
- wake-only vs contentful delivery distinctions
- cancellation of obsolete waits
- prompt-level guidance for when waiting is appropriate

The waiting plane should not own:

- shell process lifecycle
- delegated child-agent lifecycle
- high-level work identity

## Relationship To Work Items

Waiting should usually be anchored in an active work item.

The intended relationship is:

- a work item says what meaningful work exists
- the waiting plane says why that work is currently blocked
- a later timer fire or callback delivery may reactivate that work

This means waiting should help preserve continuity without turning the work
item itself into a scheduler primitive.

## Timer-Backed Waiting

Timer-backed waiting should be treated as part of the waiting plane, not as a
task substitute.

The public semantics should be:

- resume this work after a bounded delay
- optionally wake without delivering new content

This is conceptually closer to:

- deferred reactivation

than to:

- background execution

That is why `sleep_job` should not remain the long-term center of timer-based
waiting semantics.

## Callback-Backed Waiting

Callback-backed waiting should also belong to the waiting plane.

The primary public family here is:

- `CreateExternalTrigger`
- `CancelExternalTrigger`

This family is best described as:

- external trigger tools

not merely:

- callback utilities

because the important runtime meaning is not "there exists a callback URL". The
important meaning is that Holon now has a managed external ingress channel tied
to a waiting intent.

The important public contract is not:

- "here is an arbitrary callback endpoint"

The important public contract is:

- "Holon is now waiting on this future external condition"
- "here is the capability that can wake or re-enter the runtime"

This keeps callback behavior tied to waiting intent rather than making it feel
like a general-purpose ingress hack.

## External Trigger Tools

Holon should present `CreateExternalTrigger` and `CancelExternalTrigger` as one
waiting-plane sub-family with the following role:

- expose a bounded external trigger channel
- bind that channel to an active waiting intent
- distinguish wake-only reactivation from contentful re-entry
- let the agent retire stale or obsolete external wait channels

This family should remain separate from:

- command execution tools
- child-agent creation tools
- local environment tools

It is also distinct from local waiting posture:

- `Sleep` means the agent is intentionally resting now
- external trigger tools mean an outside system may wake or re-enter the agent
  later

## Wake-Only vs Contentful Reactivation

The waiting plane should preserve Holon's useful distinction between:

- wake-only reactivation
- contentful reactivation

Wake-only means:

- something changed
- the runtime should reconsider
- no new rich content is being injected directly

Contentful reactivation means:

- an external delivery contains meaningful content that should re-enter the
  queue

This distinction should remain explicit in waiting-plane semantics rather than
being buried in callback implementation details.

## Relationship To `Sleep`

`Sleep` should be understood as:

- model-owned intent to stop current active work now

It is related to waiting, but it is not the whole waiting plane.

A future shape such as delayed sleep or timer-backed sleep may still belong in
this plane, but `Sleep` alone should not be the only public story for waiting.

So within the waiting plane, Holon should be able to explain at least two
different sub-families:

- local waiting posture such as `Sleep`
- external trigger tools such as `CreateExternalTrigger` and
  `CancelExternalTrigger`

## Migration Direction

A safe initial direction is:

1. document callback and timer semantics explicitly as the waiting plane
2. stop describing delayed resumption mainly through task language
3. ensure prompt guidance tells the model to anchor cross-turn waits in work
   items before the turn ends
4. treat `sleep_job` as transitional/runtime-oriented language rather than the
   final public abstraction

## Open Questions

The following questions remain open after this RFC:

- should Holon eventually expose a more explicit timer creation tool in the
  waiting plane, or keep timer-backed waiting behind `Sleep` and control-plane
  surfaces longer?
- should waiting intent creation ever be more direct than today's callback
  capability model?
- how much waiting state should appear by default in model-facing summaries
  versus on-demand inspection tools?

## Summary

Holon should treat waiting as its own public tool plane.

The waiting plane should unify:

- timer-backed reactivation
- callback-backed reactivation
- wake-only vs contentful re-entry
- cancellation of obsolete waits

And within that plane, `CreateExternalTrigger` and `CancelExternalTrigger`
should be described explicitly as external trigger tools rather than as generic
callback utilities.

This gives Holon a cleaner public story for blocked work and future resumption.
