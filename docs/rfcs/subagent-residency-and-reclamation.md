---
title: "RFC: Subagent Residency And Reclamation"
date: 2026-10-09
status: draft
related_issue: 3430
---

# RFC: Subagent Residency And Reclamation

Handle: `rfc-subagent-residency-and-reclamation`

## Decision

The supervising parent decides when a reusable child is no longer needed and
requests its deletion directly. Background maintenance discovers unresolved
cleanup responsibilities and reminds the parent. The runtime validates and
executes deletion, including recovery from failure.

Independently, the runtime may retire a safely idle execution instance while
preserving the child's identity and durable state for later invocation.
Creation requires no retention duration. There is no mandatory keep/renewal
action, structured review response, or monitoring agent.

## Responsibilities

| Owner | Responsibility |
| --- | --- |
| Parent | Reuse a child or directly request deletion after its purpose ends |
| Background maintenance | Discover unresolved responsibility, deliver bounded reminders, and retire safely idle runtime instances |
| Runtime | Enforce ownership and protection, confirm resource exit, and execute durable deletion |
| Operator | Resolve orphaned responsibility, persistent blockers, and exceptional resource growth |

## Lifecycle Contract

- A retained child's invocation completing does not delete its identity or
  require its runtime to remain resident. Existing explicit one-shot terminal
  deletion remains supported.
- Retiring an idle instance preserves identity, supervision, conversation,
  workspace, and artifacts. It is distinct from lifecycle Stop and deletion.
  Later invocation loads an instance for new work; read-only inspection does
  not. Retirement is permitted only when owned execution and wake obligations
  can be preserved safely and the retiring instance has actually exited.
- A parent may request deletion during normal work without receiving a reminder
  first. Authorization follows current lifecycle supervision, not lineage,
  visibility, name, or permission to inspect/message the child.
- Parent cleanup must preserve executing, queued, waiting, and open work, other
  current uses, descendant responsibilities, and protected resources. Cleanup
  cannot cancel legitimate work merely to make the child eligible for deletion.
- Admission must serialize with new work and resource-reference changes. Stale
  observations or references to a previous identity incarnation cannot authorize
  deletion. Retirement must likewise preserve concurrently accepted work and
  prevent overlapping runtime instances.
- Parent task results remain readable after child deletion. Referenced artifacts
  must survive through durable ownership or explicitly block deletion. A
  historical identity reference alone does not require permanent live residency.
- An accepted deletion request is durable and idempotent. Failure and restart
  preserve its remaining cleanup responsibility. Logical task completion,
  confirmed resource exit, identity deletion, and evidence retention are distinct
  facts; identity deletion does not imply history purge.

## Forgotten And Orphaned Children

Supervision persists the cleanup responsibility independently of parent model
memory. Settled work, long inactivity, or resource growth may prompt a compact
reminder. The reminder itself grants no deletion authority.

Reminders use ordinary authorized scheduling, preserve their internal origin,
respect stopped posture and budgets, and are deduplicated and rate-limited.
They do not reopen completed parent work or require a standardized answer.
Only an authorized lifecycle operation requests deletion.

No answer leaves the child retained; safely idle instances may still retire.
Parent crash/restart preserves supervision. Parent deletion with unresolved
children exposes an explicit cleanup obligation for authorized recovery or
operator handling. Persistent failures and significant accumulation become
visible without repeated unchanged notifications.

## Scope And Related Documents

This contract concerns ephemeral, supervision-attached children and preserves
the current public/independent-agent exclusion. It introduces no creation-time
TTL, automatic identity expiration, implicit ownership transfer, or expansion
of cascade authority. Automatic identity-retention policy is separate work.

- [Identity and supervision](./agent-identity-relations-and-message-delivery.md)
- [Deletion lifecycle](./agent-deletion-lifecycle.md)
- [Lifecycle control](./agent-lifecycle-control-posture.md)
- [Host activation and durable reads](./runtime-host-activation-admission-and-read-projections.md)
- [Evidence retention](./runtime-db-retention.md)
- [Implementation research and plan (Chinese)](../rfc-implementation-notes/subagent-reclamation-plan.md)
