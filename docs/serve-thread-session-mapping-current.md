# Serve Thread/Session Mapping (Current Implementation)

This document describes the current (code-as-implemented) mapping between:

- RPC `thread_id` (TUI/control-plane visible)
- internal `sessionKey` (controller routing lane)
- controller runtime agent session (background long-lived session)

It also summarizes how messages are delivered to TUI/main thread.

## Scope

- Source of truth: current code paths in:
  - `cmd/holon/serve.go`
  - `pkg/serve/webhook.go`
  - `pkg/serve/control.go`
  - `pkg/tui/app.go`
- This is a behavior snapshot, not a target design.

## Terms

- `thread_id`: control-plane conversation id used by `turn/start` and TUI.
- `sessionKey`: internal controller routing key used for queueing, locking, and controller RPC.
- `controller session`: the single long-lived agent runtime process/container used by serve.
- `main`: default runtime session id and default user-facing thread.

## Identity Mapping

### 1) Runtime default thread/session

- Serve runtime initializes with default session id `main`.
- This is what stream subscribers first see as `thread/started`.

Code:
- `pkg/serve/control.go:123`
- `pkg/serve/control.go:151`
- `pkg/serve/webhook.go:442`

### 2) RPC turn mapping (`turn/start`)

For interactive RPC turns:

- incoming `req.ThreadID` is normalized
- empty -> `main`
- `sessionKey = threadID`
- event converted to `source=rpc`, `type=rpc.turn.input`
- `scope.partition = threadID`
- `subject = {kind:"thread", id:threadID}`

Code:
- `cmd/holon/serve.go:801`
- `cmd/holon/serve.go:834`

Result: for RPC/TUI, mapping is effectively `thread_id == sessionKey`.

### 3) External event mapping (GitHub/timer/etc.)

Non-RPC events use `routeEventToSessionKey(env)`:

Priority:
1. payload `session_key` (or payload `thread_id`)
2. `scope.partition` -> `event:<partition>`
3. `scope.repo` -> `event:<repo>`
4. `source+subject.kind+subject.id` -> `event:<...>`
5. `source+type` -> `event:<...>`
6. fallback `main`

Code:
- `cmd/holon/serve.go:1819`
- `cmd/holon/serve.go:1842`

So GitHub events commonly route to `event:<repo-or-partition>`, not `main`.

## Controller Runtime Session Model

Important: serve currently uses one controller runtime session process, not one process per `sessionKey`.

- `cliControllerHandler` holds one `controllerSession` pointer.
- all `sessionKey`s are multiplexed through that runtime via `/v1/runtime/events`.

Code:
- `cmd/holon/serve.go:594`
- `cmd/holon/serve.go:1594`
- `cmd/holon/serve.go:2297`

Per-`sessionKey` isolation is implemented by queue/lock policy in host process:

- per-session lock map: `sessionLocks[sessionKey]`
- global concurrency semaphore: `maxConcurrent`
- per-session queued turn bookkeeping and epoch skipping policies

Code:
- `cmd/holon/serve.go:977`
- `cmd/holon/serve.go:1042`
- `cmd/holon/serve.go:1115`

## Message Delivery Mechanisms

There are three distinct delivery paths:

### A) Ingress notification (`event/received`)

- emitted immediately when envelope is accepted
- includes `event_id/source/event_type/repo`
- not tied to a turn

Code:
- `pkg/serve/webhook.go:564`
- `pkg/serve/webhook.go:639`

### B) Turn lifecycle notifications (`turn/*`, `item/*`)

For RPC turns:

1. `turn/start` creates active turn in runtime
2. controller dispatch produces ack/progress records
3. webhook consumes turn ack channel
4. runtime emits:
   - `turn/progress`
   - terminal `turn/completed` or `turn/interrupted`
   - optional assistant `item/created` from ack message

Code:
- `pkg/serve/control.go:978`
- `cmd/holon/serve.go:1226`
- `pkg/serve/webhook.go:264`
- `pkg/serve/control.go:868`
- `pkg/serve/control.go:909`

### C) Background-to-main announce (`session.announce`)

When controller processes non-main session event and returns:

- host builds synthetic event:
  - `source=serve`
  - `type=session.announce`
  - routed to `sessionKey=main`
- webhook only emits main-thread item for this type
- `decision=no-op` is explicitly filtered (not emitted)

Code:
- `cmd/holon/serve.go:1277`
- `cmd/holon/serve.go:1285`
- `pkg/serve/webhook.go:686`
- `pkg/serve/webhook.go:705`

This is the key bridge from background event sessions to visible main timeline.

## End-to-End Flows

### Flow 1: Interactive TUI/RPC turn

1. TUI sends `turn/start(thread_id=main, ...)`.
2. runtime creates turn id.
3. handler wraps into `rpc.turn.input` with `sessionKey=main`.
4. controller RPC executes, status polled.
5. turn ack updates produce `turn/progress` -> `turn/completed`.

Primary code:
- `pkg/tui/app.go:1277`
- `pkg/serve/control.go:978`
- `cmd/holon/serve.go:801`
- `cmd/holon/serve.go:1194`

### Flow 2: GitHub event

1. webhook normalizes event envelope (`source=github,...`).
2. `sessionKey` routed to `event:*`.
3. controller handles event in that session lane.
4. `events/decisions/actions` append records regardless of UI visibility.

Primary code:
- `cmd/holon/serve.go:790`
- `cmd/holon/serve.go:1819`
- `pkg/serve/webhook.go:560`

### Flow 3: GitHub event -> main visible summary

1. Flow 2 completes.
2. if source lane != `main`, enqueue `session.announce` to `main`.
3. webhook `maybeEmitSessionAnnounce` converts to main `item/created`.
4. TUI renders as conversation/activity item.

Primary code:
- `cmd/holon/serve.go:1277`
- `cmd/holon/serve.go:1317`
- `pkg/serve/webhook.go:686`

## Why "processed but not seen in main" can happen

Current implementation allows this scenario:

- event is processed (`actions.ndjson` shows `status=ok`)
- but main visibility depends on announce path quality:
  - not routed to `session.announce`, or
  - announce filtered as no-op, or
  - announce content is weak/empty and appears confusingly

So "processed" truth should be checked in state logs, not only TUI timeline.

## Operational Logs and State Files

Under agent home state (example: `~/.holon/agents/main/state`):

- `events.ndjson`: accepted envelopes
- `decisions.ndjson`: dedupe/forward decisions
- `actions.ndjson`: handler execution result (`ok/skipped/failed`)
- `runtime-state.json`: runtime `SessionID`, counters
- `controller-state/turn-event-index.json`: `turn_id -> event_id`
- `controller-state/controller-session.json`: controller session metadata
- `controller-state/claude-config/projects/...jsonl`: controller conversation/event payload trace

## Practical Correlation Keys

Use these ids to correlate across layers:

- `event_id`: event lifecycle (`events` -> `actions` -> controller status)
- `turn_id`: UI turn lifecycle (`turn/start` -> `turn/progress` -> terminal)
- `thread_id`: user-facing conversation
- `sessionKey`: controller lane
- `source_session_key` (announce payload): background lane origin for main summary

## Current Mapping Summary Table

| Input kind | thread_id | sessionKey | Visible by default in main |
|---|---|---|---|
| RPC `turn/start` | user provided (default `main`) | same as `thread_id` | yes (turn lifecycle) |
| GitHub webhook event | n/a | usually `event:*` | no (unless announce emitted) |
| Synthetic `session.announce` | `main` | `main` | yes (as `item/created`) |

