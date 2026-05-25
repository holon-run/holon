---
title: Agent state
summary: Current agent state, lifecycle labels, runtime projection, and user-facing display contract.
order: 10
---

# Agent state

This page defines the current contract for agent state, lifecycle, and
runtime projection in Holon. It is verified against implementation and tests
as of the last review date noted below.

> **Last verified:** 2026-05-23 against `src/types.rs` `AgentState`,
> `AgentStatus`, `AgentIdentityView`, `AgentSchedulingPosture`,
> `AgentPostureProjection`, `ClosureDecision`, `ContinuationResolution`,
> `RuntimePosture`, and `AgentSummary`.

## Source RFCs

- [Agent State Model And Runtime Projection](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-state-model.md)
- [Agent Lifecycle Control Posture](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-lifecycle-control-posture.md)
- [Agent Control Plane Model](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-control-plane-model.md)
- [Agent Profile Model](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-profile-model.md)
- [Agent Initialization and Template](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-initialization-and-template.md)

## Authoritative records vs projections

Agent state is **derived** from authoritative runtime records, not stored as a
single opaque status field. The key distinction is:

| Layer | What | Authority |
|-------|------|-----------|
| **Identity** | `agent_id`, kind, visibility, ownership, profile preset | Agent registry record |
| **Lifecycle status** | `AgentStatus` — Booting, AwakeIdle, AwakeRunning, AwaitingTask, Asleep, Stopped | Scheduler executor (single writer) |
| **Scheduling posture** | `AgentSchedulingPosture` — derived from queue, WorkItems, tasks, wait state | Scheduler `derive_posture` projection |
| **Runtime posture** | `RuntimePosture` — Awake or Sleeping | Closure decision at turn end |
| **Continuation** | `ContinuationResolution` — how the agent was reactivated | Ingress/dispatch at turn start |
| **User-facing summary** | `AgentSummary` — stable projection for API/UI/model display | `AgentGet` tool + HTTP `/agents` |

`AgentSummary` is a **display projection**, not the source of truth for
scheduling decisions. The scheduler must derive posture from queue, WorkItem,
task, and wait state, not from summary fields.

## Agent lifecycle status (`AgentStatus`)

```text
                ┌─────────────┐
                │   Booting   │
                └──────┬──────┘
                       │ daemon_start / Start
                       ▼
                ┌─────────────┐
         ┌─────►│  AwakeIdle  │◄─────────────┐
         │      └──────┬──────┘              │
         │             │ turn starts         │
         │             ▼                     │
         │      ┌──────────────┐             │
         │      │ AwakeRunning │             │
         │      └──────┬───────┘             │
         │             │ closure: Sleep      │
         │             ▼                     │
         │      ┌─────────────┐     ┌────────┴──────┐
         ├──────│    Asleep    │────►│  AwaitingTask │
         │      └─────────────┘     └────────┬──────┘
         │         wake / resume              │ task result
         │                                    │
         └────────────────────────────────────┘

                          Stop ──► ┌──────────┐
                                   │  Stopped  │
                                   └──────────┘
```

| Status | Meaning |
|--------|---------|
| `Booting` | Agent is initializing; not yet handed to the scheduler |
| `AwakeIdle` | Agent is awake but no model turn is in progress |
| `AwakeRunning` | A model turn is currently executing |
| `AwaitingTask` | Agent is awake but blocked on a non-terminal task result |
| `Asleep` | Agent called `Sleep` at end of turn; no model turn running |
| `Stopped` | Agent lifecycle is stopped; scheduler will not start new turns |

**Key contract:**

- `Sleep` sets status to `Asleep` — it is a turn-end posture, not an
  authoritative "idle" declaration.
- An `Asleep` agent can have runnable WorkItems. `Asleep` does **not** mean
  idle or empty.
- `AwaitingTask` means a non-terminal task (command, child agent) blocks
  further model reentry; the agent is still awake and the scheduler can wake
  it when the task completes.
- `Stopped` is a hard lifecycle boundary. The scheduler will not start new
  turns for a stopped agent. Stopped agents release runtime-owned execution
  resources.
- Status transitions flow through scheduler-owned helpers; no module should
  directly mutate `AgentState.status` without going through the scheduler
  executor.

## Scheduling posture (`AgentSchedulingPosture`)

The scheduler derives a scheduling posture from current state. This is a
**projection**, not stored state:

| Posture | Condition |
|---------|-----------|
| `ActiveTurn` | A model turn is currently running |
| `HasQueuedInput` | Queue contains pending operator messages |
| `HasRunnableWork` | At least one WorkItem is runnable |
| `WaitingForTask` | An active non-terminal task is blocking |
| `WaitingForExternal` | Agent is waiting on an external event |
| `WaitingForOperator` | WorkItem `plan_status=needs_input` |
| `Blocked` | WorkItem has `blocked_by` set |
| `Idle` | No queued input, no runnable work, no blocking conditions |
| `Unknown` | Default before first projection; not part of the stable contract |
| `Archived` | Agent lifecycle is stopped (maps from `AgentStatus::Stopped`) |

**Key contract:**

- Posture is derived from queue depth, WorkItem readiness, waiting state,
  task blocking state, and external triggers.
- Posture is snapshot-derived; it is not persisted as durable state.
- `AgentSummary.scheduling_posture` exposes this projection; consumers should
  not treat it as an authoritative scheduling input.

## Closure and continuation

At the end of each turn, the closure decision determines next posture:

| `ClosureOutcome` | Effect |
|------------------|--------|
| `Completed` | Work completed; agent ready for next work |
| `Continuable` | Work continues; same WorkItem remains active |
| `Failed` | Turn failed; agent can recover or escalate |
| `Waiting` | Agent is waiting for operator, external, task, or timer |

When a waiting agent is reactivated, `ContinuationResolution` records:

| Field | Meaning |
|-------|---------|
| `trigger_kind` | OperatorInput, TaskResult, ExternalEvent, TimerFire, InternalFollowup, SystemTick |
| `class` | ResumeExpectedWait, ResumeOverride, LocalContinuation, LivenessOnly |
| `model_reentry` | Whether the model should be re-entered with context |
| `matched_waiting_reason` | Whether the trigger matched the prior waiting reason |

## User-facing projection (`AgentSummary`)

`AgentSummary` is the stable projection returned by `AgentGet` and
`GET /agents`. It includes:

- `identity` — agent identity badge (visibility/ownership/profile)
- `agent` — core `AgentState` including status, pending count, turn index
- `scheduling_posture` — derived posture snapshot
- `lifecycle` — lifecycle hint (not authoritative)
- `model` — current model selection and token usage
- `closure` — last closure decision
- `execution` — execution snapshot (run id, cwd, workspace)
- `active_children` — visible child agent summaries
- `active_waiting_intents` / `active_wait_conditions` — current wait state
- `active_external_triggers` — provisioned external ingress capabilities

**Key contract:**

- `AgentSummary` is assembled on read from authoritative records.
- Fields are added conservatively; do not use `AgentSummary` as a dumping
  ground for internal state.
- The model receives `AgentSummary` (via `AgentGet`) as display information,
  not as a scheduling instruction.
- API consumers must not depend on summary field ordering or presence of
  default/empty fields.

## Lifecycle control

Agent lifecycle control is `Start` / `Stop`:

- `Start` hands the agent to the scheduler. It does **not** directly start a
  model turn; the scheduler decides whether the agent should be idle or
  process queued input.
- `Stop` aborts the current run, releases runtime-owned execution resources,
  and marks the agent as not runnable. Queued messages and durable records
  are preserved.
- There is no `Pause` / `Resume`. `Stop` + `Start` is the contract.

## Known gaps

- `AgentStatus` still includes transitionary states (`Booting`, `AwaitingTask`)
  that may collapse as the scheduler model matures. See follow-up if
  `AwaitingTask` becomes fully subsumed by `AwakeIdle` + task blocking.
- `AgentLifecycleHint` is under-defined; its relationship to `AgentStatus` and
  `AgentSchedulingPosture` is not yet a stable contract.
- `AgentSummary` includes some fields (`recent_operator_notifications`,
  `recent_brief_count`) whose contract is not yet hardened.
- `AgentStatus::Asleep` remains a lifecycle/display projection, but scheduler
  idle-boundary decisions inspect wait and work facts before treating an
  already-asleep agent as idle.
- `AgentStatus::AwaitingTask` exists in code but is not in the RFC's target
  status set (`agent-lifecycle-control-posture.md`).
- `AgentLifecycleHint` retains `resume_*` fields from the deprecated Pause/Resume
  model. See [issue #1378](https://github.com/holon-run/holon/issues/1378).
- `AgentSchedulingPosture::Archived` is used (for `Stopped` agents) contrary
  to the previous spec claim of "Not currently used".
