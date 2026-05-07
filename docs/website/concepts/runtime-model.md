---
title: Runtime model
summary: How Holon separates agents, work items, tasks, queues, and delivery.
order: 10
---

# Runtime model

Holon treats agent execution as a runtime problem. A turn is important, but it
is not the whole system. The runtime tracks durable identity, active work,
supervised tasks, wake conditions, and final delivery as separate concerns.

## Agents

An agent is an addressable runtime actor. It has an identity, lifecycle, active
workspace, loaded guidance, and durable local state. Agents can be public and
self-owned, or private and supervised by a parent task.

## Work items

A work item names an objective that may outlive a single model turn. It carries
the plan, readiness state, blockers, and a progress checklist. This lets Holon
resume or inspect work without pretending that every continuation is a new
chat message.

## Tasks

Tasks are supervised executions. A task can be a shell command, a long-running
process, or a delegated child agent. Task lifecycle is separate from the
agent's user-facing answer: Holon can inspect status, read bounded output, send
continuation input, or stop work explicitly.

## Queues and wakeups

Holon can enqueue follow-up messages, sleep when no immediate work remains, and
wake on external triggers. These state transitions are visible in the runtime
surface so integrations do not need to infer hidden background behavior.

## Delivery

Holon separates internal execution traces from user-facing delivery. A final
answer should explain the useful result, while logs, command output, and
runtime evidence remain available through the appropriate task or memory
surfaces.

## The operating loop

1. Ingress arrives with origin, trust, and priority metadata.
2. The agent anchors the objective as a work item when the work is non-trivial.
3. The agent reads only the context needed to act safely.
4. Mutations happen through explicit workspace or task tools.
5. Verification runs through real project checks when available.
6. The agent delivers a concise result and sleeps if no follow-up remains.

This loop keeps Holon headless and integration-friendly while still supporting
long-lived, stateful work.
