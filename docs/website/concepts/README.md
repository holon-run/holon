---
title: Concepts
summary: Runtime concepts that define Holon's execution model.
order: 20
---

# Concepts

Holon is easiest to understand as a runtime rather than as a chat wrapper. The
core concepts below describe what the runtime makes explicit. Start with the
runtime model, then read trust boundaries before wiring the system into
external inputs or automation.

Concept pages explain behavior; guides show workflows; reference pages describe
the current surface area.

## Core ideas

- Agents have durable homes and can be addressed across turns.
- Work items name objectives separately from transient model context.
- Tasks represent supervised execution, including shell commands and delegated
  child agents.
- Queues, wake hints, sleeps, and external triggers are visible lifecycle
  mechanisms instead of hidden background behavior.
- Origin, trust, and priority remain attached to ingress and execution events.

<!-- INDEX:START -->

- [Runtime model](./runtime-model.md)
  How Holon separates agents, work items, tasks, queues, and delivery.
  <!-- mdorigin:index kind=article -->

- [Trust boundaries](./trust-boundaries.md)
  How Holon preserves provenance across operator input, external events, and delegated output.
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
