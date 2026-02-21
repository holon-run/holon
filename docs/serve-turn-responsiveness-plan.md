# `holon serve` Turn Responsiveness Improvement Plan

## Why this doc

Current interactive experience has two major gaps:

1. During long turns, users do not see meaningful in-progress updates.
2. Interrupt exists in API (`turn/interrupt`) but cancellation is not reliably propagated to active backend work.

This document defines a phased implementation plan to improve perceived and actual responsiveness.

## Current behavior and bottlenecks

### Control-plane flow

- Incoming `turn/start` is dispatched to controller runtime via queued event dispatch in `cmd/holon/serve.go`.
- Per-session lock is held across full event dispatch + status polling:
  - `cmd/holon/serve.go:809`
  - `cmd/holon/serve.go:820`
  - `cmd/holon/serve.go:918`
- Status polling waits for terminal state (`completed`/`failed`) and only then publishes turn ack:
  - `cmd/holon/serve.go:1654`

### Timeout profile

- Default controller event timeout is 60 minutes:
  - `cmd/holon/serve.go:52`

### TUI UX

- TUI supports `pause/resume` but does not expose a direct turn interrupt action:
  - keys in `pkg/tui/app.go:337` and `pkg/tui/app.go:340`
- Active turn display falls back to `...` when no assistant item has arrived:
  - `pkg/tui/app.go:1043`

### Interrupt semantics today

- `turn/interrupt` exists and changes runtime turn state:
  - `pkg/serve/control.go:971`
- But targeted interrupt does not guarantee backend controller work is canceled.

## Target outcomes

1. Users always see liveness during long turns.
2. Interrupt is actionable and fast (ack + best-effort backend cancellation).
3. Control commands (interrupt/steer) are not blocked behind long-running data turns.
4. Queue behavior under burst input is predictable (no apparent "hang").
5. Long/parallelizable work is delegated to subagents so the main session stays responsive.

## Design principles

1. Acknowledge early, complete later.
2. Separate control-path latency from data-path latency.
3. Emit explicit non-terminal progress states.
4. Make cancellation observable (requested, accepted, completed, failed).

## Phase 1: Liveness and UI feedback (lowest-risk, immediate UX win)

### 1. Add non-terminal progress notifications

Add `turn/progress` notification (new) with payload similar to:

```json
{
  "turn_id": "turn_xxx",
  "thread_id": "main",
  "state": "queued|running|waiting|cancel_requested",
  "message": "controller event status: running",
  "event_id": "evt_xxx",
  "updated_at": "2026-02-21T10:00:00Z",
  "elapsed_ms": 18420
}
```

Emit from polling loop in `waitForControllerEventResult` on:

- status transition
- periodic heartbeat while status unchanged

Reference polling code:

- `cmd/holon/serve.go:1654`

### 2. Improve TUI rendering for active turns

In TUI:

- handle `turn/progress` notification
- replace `Agent ...` placeholder with last progress text + elapsed
- show explicit state labels (`Queued`, `Running`, `Cancel Requested`)

Current placeholder behavior:

- `pkg/tui/app.go:1043`

### 3. Expose interrupt action in TUI

Add keybinding for targeted interrupt of current active turn.

- Keep existing `Ctrl+P`/`Ctrl+R` behavior (runtime pause/resume)
- Add dedicated key for `turn/interrupt` with active turn id

## Phase 1.5: Subagent-first orchestration for long work

### Why this is needed

- Current serve controller prompt does not explicitly instruct "delegate long tasks to subagents":
  - `cmd/holon/serve.go:1202`
- Agent runtime currently sets `allowedTools: ["Skill"]` in SDK options:
  - `agents/claude/src/agent.ts:508`
  - `agents/claude/src/agent.ts:707`
- In current `permissionMode: "bypassPermissions"` flow, `allowedTools` does not act as a tool-availability whitelist; it should not be used to model subagent capability.

### 1. Prompt policy update for serve controller

Extend `defaultControllerRuntimeUserPrompt` with explicit delegation rules:

1. Main session acts as orchestrator and should acknowledge quickly.
2. If task is long-running or parallelizable, prefer subagent execution.
3. Do not busy-poll child status; rely on completion push/announce.
4. Keep parent turn responsive for steer/interrupt/control operations.

### 2. SDK/tool policy update

For serve session turns, make tool policy explicit and remove ambiguous settings:

1. Remove `allowedTools` from serve SDK options while keeping `permissionMode: "bypassPermissions"`.
2. Use `tools`/`disallowedTools` (not `allowedTools`) to define actual available tool surface.
3. Ensure subagent path is available via `Task` (and `TaskOutput` if background child execution is used).
4. Optionally provide constrained `agents` definitions for common worker roles.
5. If we later move away from bypass mode, re-introduce `allowedTools` only as an auto-approve UX optimization.

### 3. Parent-child execution contract

Define runtime behavior when controller delegates:

1. Parent turn records child task IDs and transitions to running/orchestrating state.
2. Child completion is pushed back as parent progress/item update.
3. Parent can finish early with interim summary or wait for required children (policy-driven).

### 4. Safety limits

Start with conservative limits:

- max spawn depth = 1
- max active children per parent turn/session
- dedupe repeated child tasks
- on parent interrupt, cascade cancel to active child tasks

## Phase 2: Real cancellation semantics

### 1. Track `turn_id -> event_id`

Persist mapping once controller accepts/queues an event. This enables targeted cancellation later.

### 2. Backend cancellation hook

When `turn/interrupt` is called with `turn_id`:

- mark turn as `cancel_requested`
- issue cancellation to controller runtime (new RPC/HTTP action)
- emit progress updates until terminal state

If backend cannot cancel, surface explicit failure reason in turn notification.

### 3. Queue cleanup on interrupt

On targeted interrupt, clear same-session queued follow-up work that has not started.

## Phase 3: Scheduling and queue governance

### 1. Separate control lane from data lane

Current session lock serializes all work under one lane. Move to:

- control lane: interrupt/steer/status-critical operations
- data lane: normal turn execution

Control lane should preempt queueing delay from long data tasks.

### 2. Add follow-up queue policy

Introduce per-session queue policy options:

- `followup`
- `interrupt`
- `collect`
- dedupe + cap + drop policy

Goal: avoid "dead air + backlog pile-up" during bursts.

## Timeout policy changes

### Decision

- Keep default total controller event timeout unchanged at 60 minutes:
  - `cmd/holon/serve.go:52`
- Responsiveness issues should be addressed by progress visibility, control-path isolation, and subagent delegation, not by shrinking default timeout.
- Timeout tuning remains optional deployment policy via existing `HOLON_SERVE_EVENT_TIMEOUT`.

## Testing plan

### Unit and integration

1. `turn/progress` emitted on status transition and heartbeat.
2. TUI displays progress and elapsed updates.
3. TUI interrupt key triggers `turn/interrupt` RPC with active turn id.
4. Interrupt path transitions:
   - `running -> cancel_requested -> interrupted`
   - failure path includes reason
5. Session queue cleanup validated after interrupt.
6. Subagent path:
   - long-task prompt triggers subagent delegation
   - parent turn remains responsive during child execution
   - child completion is pushed back without polling loops
   - parent interrupt cascades to child tasks

### Manual smoke checks

1. Start long-running turn; verify progress every few seconds.
2. Interrupt from TUI; verify immediate UI acknowledgment.
3. Confirm backend task stops (or explicit cancel-failed reason shown).
4. Send new turn after interrupt; ensure prompt acceptance.

## Rollout strategy

Single PR, multiple commits (recommended review order):

1. Commit 1: `turn/progress` + TUI progress rendering + TUI interrupt key.
2. Commit 2: subagent prompt policy + SDK Task/TaskOutput enablement + parent/child state plumbing.
3. Commit 3: backend cancellation propagation + turn/event mapping + child cancel cascade.
4. Commit 4: control/data lane split + follow-up queue policies.

Keep protocol additions backward-compatible:

- clients ignoring `turn/progress` still work
- existing `turn/start`, `turn/interrupt` unchanged

## Open questions

1. What backend cancellation primitive should controller runtime expose (cancel by event id vs turn id)?
2. Should interrupted turns preserve partial assistant output by default?
3. For non-targeted pause, should in-flight turns be canceled or only stop new intake?
4. Should parent turn completion wait for all child tasks or allow partial completion with late child announce?
5. Should we define fixed worker subagent profiles (`researcher`, `implementer`, `tester`) in SDK `agents`, or begin with generic Task-only delegation?

## Implementation anchors

- Event dispatch and polling: `cmd/holon/serve.go`
- Runtime turn lifecycle: `pkg/serve/control.go`
- Notification contract: `docs/serve-notifications.md`
- TUI behavior: `pkg/tui/app.go`, `pkg/tui/client.go`
