---
title: Agent state
summary: Current agent state, lifecycle labels, runtime projection, and user-facing display contract.
order: 10
---

# Agent state

This page defines the current contract for agent state, lifecycle, and
runtime projection in Holon. It is verified against implementation and tests
as of the last review date noted below.

> **Last verified:** 2026-05-27 against `src/types.rs` `AgentState`,
> `AgentStatus`, `AgentIdentityView`, `AgentSchedulingPosture`,
> `AgentPostureProjection`, `ClosureDecision`, `ContinuationResolution`,
> `RuntimePosture`, and `AgentSummary`; `src/storage.rs`
> `agent_posture_projection`; `src/runtime/lifecycle.rs` `agent_summary`;
> `src/runtime/closure.rs` closure derivation; and `src/tool/tools/agent_get.rs`.

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

Current implementation anchors:

- `RuntimeHandle::agent_summary` assembles `AgentSummary` on read from the
  current `AgentState`, identity view, model state, execution snapshot, active
  waits, children, external triggers, and `AppStorage::agent_posture_projection`.
- `AgentGet` returns that assembled `AgentSummary` directly; it does not write
  lifecycle, scheduling, or wait state.
- `/agents` and `/agents/{id}` use the same runtime summary/list projection
  path. `AgentListEntry` is a compact list projection and is not a scheduler
  input.
- `AppStorage::agent_posture_projection` derives the exposed
  `AgentSchedulingPosture` from persisted/runtime records. No scheduler-sensitive
  path should read `AgentSummary.scheduling_posture` back as authority.

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
         │             │ turn closure        │
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
| `AwaitingTask` | Transitional label for an awake agent blocked on a non-terminal task result |
| `Asleep` | Runtime accepted turn closure and no model turn is running |
| `Stopped` | Agent lifecycle is stopped; scheduler will not start new turns |

**Key contract:**

- `Asleep` is a runtime posture reached only when the scheduler accepts rest
  after turn closure. It is not an authoritative "idle" declaration.
- `WaitFor` records explicit wait state before yielding; it is the preferred
  path when a WorkItem or agent is waiting on task, external, or operator
  input.
- An `Asleep` agent can have runnable WorkItems. `Asleep` does **not** mean
  idle or empty.
- `AwaitingTask` is a transitional lifecycle label used by current runtime,
  TUI, daemon, and waiting projections while a non-terminal task (command,
  child agent) blocks further model reentry. It may later collapse into
  `AwakeIdle` plus task-wait scheduling posture, but remains current contract
  until that migration happens.
- `Stopped` is a hard lifecycle boundary. The scheduler will not start new
  turns for a stopped agent. Stopped agents release runtime-owned execution
  resources.
- Status transitions flow through scheduler-owned helpers; no module should
  directly mutate `AgentState.status` without going through the scheduler
  executor.

## Scheduling posture (`AgentSchedulingPosture`)

The scheduler derives a scheduling posture from current state. This is a
**projection**, not stored state. The current reduced agent-level projection
uses this precedence:

| Posture | Condition |
|---------|-----------|
| `Archived` | Agent lifecycle is stopped (`AgentStatus::Stopped`) |
| `ActiveTurn` | `AgentState.current_run_id` is set |
| `HasQueuedInput` | Queue contains a pending queued entry for the agent |
| `HasRunnableWork` | Current or queued WorkItem is runnable |
| `WaitingForTask` | A WorkItem has an active task wait condition |
| `WaitingForExternal` | A WorkItem has an active external waiting intent |
| `WaitingForOperator` | WorkItem `plan_status=needs_input` or active operator wait |
| `Blocked` | WorkItem has `blocked_by` set or an active timer/system/non-operator wait |
| `Idle` | No queued input, no runnable work, no blocking conditions |
| `Unknown` | Default before first projection; not part of the stable contract |

**Key contract:**

- Posture is derived from queue depth, WorkItem readiness, waiting state,
  task blocking state, and external triggers.
- Posture is snapshot-derived; it is not persisted as durable state.
- Stopped lifecycle wins the exposed projection (`Archived`) before transient
  run, queue, work, or wait facts.
- Queue and runnable work outrank passive sleep posture. An `Asleep` agent with
  queued input or runnable WorkItems is projected as `HasQueuedInput` or
  `HasRunnableWork`, not `Idle`.
- WorkItem-level `WaitingTimer` and `WaitingSystem` remain distinct scheduler
  wait states, but the reduced agent-level posture currently reports them as
  `Blocked`; scheduler idle-boundary decisions still inspect the WorkItem wait
  state to emit the timer or system-tick action.
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

Closure derivation is separate from the display posture. It uses scheduler
projection facts and current turn facts to choose a `ClosureOutcome`,
`WaitingReason`, and `RuntimePosture`. In current implementation:

- explicit operator waits outrank other wait conditions for closure reason;
- blocking tasks are metadata unless represented by a current work wait;
- active work-item or agent waiting intents map to external wait;
- timers map to timer wait;
- runnable work can prevent an unrelated agent-level waiting intent from
  becoming the closure reason.

## User-facing projection (`AgentSummary`)

`AgentSummary` is the stable projection returned by `AgentGet` and
`GET /api/agents`. It includes:

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

## Validation findings

This page was validated against the RFCs and implementation areas listed in
issue #1367.

| Area | Finding | Classification | Current handling |
|------|---------|----------------|------------------|
| `AgentSummary` / `AgentGet` derivation | Summary is assembled on read; `AgentGet` is read-only and does not mutate state. | Contract matches implementation | Documented above. |
| Projection as scheduler input | The user-facing `AgentSummary.scheduling_posture` is derived from storage/runtime facts. Scheduler-sensitive closure and run-loop paths derive from queue, WorkItems, waits, tasks, and turn state rather than reading the summary back. | Contract matches implementation | Covered by storage and runtime tests. |
| Lifecycle labels | Current implementation preserves `Booting`, `AwakeIdle`, `AwakeRunning`, `AwaitingTask`, `Asleep`, and `Stopped`; `Paused` only deserializes as legacy alias for `Stopped`. | Contract matches implementation with transitional label | Documented as current contract and known migration gap. |
| Agent-level timer/system waits | WorkItem scheduling keeps `WaitingTimer` and `WaitingSystem` distinct, while reduced `AgentSchedulingPosture` reports them as `Blocked`. | Intentional reduced projection | Documented; tests cover the projection. |
| `Archived` posture | `AgentSchedulingPosture::Archived` is used for stopped agents. | Stale prior spec wording | Corrected in this page. |
| Durable state vs runtime projection | `AgentState`, queue entries, WorkItems, tasks, wait conditions/intents, external triggers, and audit/transcript records remain authoritative; `AgentSummary` remains display/API projection. | Contract matches implementation | Documented as layer table and anchors. |

## Known gaps

- `AgentStatus` still includes transitionary states (`Booting`, `AwaitingTask`)
  that may collapse as the scheduler model matures. See follow-up if
  `AwaitingTask` becomes fully subsumed by `AwakeIdle` + task blocking.
- `AgentLifecycleHint` carries lifecycle delivery hints such as whether
  external messages are accepted and optional operator guidance. Deprecated
  Pause/Resume projection fields are not part of the contract.
- `AgentSummary` includes some fields (`recent_operator_notifications`,
  `recent_brief_count`) whose contract is not yet hardened.
- `AgentStatus::Asleep` remains a lifecycle/display projection, but scheduler
  idle-boundary decisions inspect wait and work facts before treating an
  already-asleep agent as idle.
- `AgentStatus::AwaitingTask` remains a transitional status in code even though
  it is not in the long-term target status set
  (`agent-lifecycle-control-posture.md`).
- `AgentSchedulingPosture::Archived` is used (for `Stopped` agents) contrary
  to the previous spec claim of "Not currently used".
