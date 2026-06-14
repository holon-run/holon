# Agent Status Display Projection

## Status

Draft.

## Context

Holon already separates durable runtime state from derived agent posture in
[Agent State Model And Runtime Projection](./agent-state-model.md). The next
boundary to make explicit is the display contract used by clients such as the
TUI and Web GUI.

Without a shared display projection, clients tend to combine low-level
lifecycle, scheduling posture, queue counters, task counters, and waiting
details independently. That produces confusing states such as a single agent
appearing as both `Ready` and `Waiting`, or as both `Stopped` and `Running`.

## Decision

User-facing agent lists should display one primary status per agent. That
status is a display projection, not a new canonical runtime state.

The projection is derived from these inputs:

1. `AgentSchedulingPosture` / `scheduling_posture` as the primary semantic
   source.
2. Compact attention counters such as queued operator input and active tasks as
   compatibility inputs while all API clients migrate to the posture contract.
3. `AgentStatus` / lifecycle only as a fallback or secondary detail.
4. Waiting conditions and waiting intents as explanatory detail, not as a
   second primary badge.

## Display precedence

The primary display status should use this precedence:

1. Stopped or archived lifecycle/posture, unless a higher-priority recovery or
   error state is explicitly defined.
2. Needs operator input / queued input.
3. Active turn or active runtime task.
4. Runnable work.
5. Waiting for operator, external wake, task result, timer, or system wake.
6. Blocked.
7. Idle / ready.
8. Unknown.

This is intentionally close to the derived scheduling posture precedence, but it
collapses adjacent runtime distinctions into stable labels that are easy to
scan in a navigation list.

## UI contract

Agent list rows should:

- show at most one primary status badge;
- prefer posture-derived labels such as `Needs input`, `Running`, `Runnable`,
  `Waiting`, `Blocked`, `Stopped`, or `Ready`;
- put lifecycle, current work state, and waiting/task counters in secondary
  text or tooltips;
- avoid independently rendering lifecycle and attention badges side by side;
- avoid treating waiting as idle. Waiting means the runtime has an explicit
  wake condition or wait intent.

Agent detail views may show more state, but should keep these axes visually
separate:

- lifecycle: process/control state;
- scheduling posture: what the agent can do next;
- waiting reason: why the scheduler is paused;
- work/task details: concrete current activity.

## API expectation

`/agents/list` should eventually expose enough compact summary fields for
clients to render the primary display status without issuing one `/state`
request per agent. A detail page may fetch the selected agent's full state.

During migration, clients may compute the display status from the existing
summary fields, but should keep that computation centralized and aligned with
this RFC.

## Web GUI migration

The Web GUI should first update the sidebar agent row:

- replace the lifecycle badge plus attention badge pair with one primary badge;
- derive that badge from posture plus compact summary counters;
- move lifecycle/current work information to the secondary metadata line and
  tooltip.

This is a display-only step. It should not change scheduler behavior or the
runtime's canonical posture derivation.
