# Session Keys, Threads, and Event Routing (Draft)

This document proposes a unified session mechanism for `holon serve` and its control plane.

Goals:
- Give users a stable, simple mental model: **threads** in the RPC/TUI map to **session keys**.
- Ensure long-running `serve` does not suffer from **context explosion** by isolating execution contexts.
- Preserve **observability**: background events remain visible to users via a main "inbox" thread.
- Keep the design **transport-agnostic**: GitHub is an event source, not a first-class concept.

Non-goals:
- Backward compatibility with unpublished flags/config layouts.
- Defining a single "best" TUI; this is about server-side session semantics.

## Terminology

- **Thread**: user-facing conversation identifier used by the control plane (RPC/TUI). Today this is `thread_id`.
- **Session key**: internal routing key for isolation, concurrency, and persistence. Conceptually similar to OpenClaw `sessionKey`.
- **Engine session id**: provider/runtime-specific identifier (e.g. Claude SDK `session_id`) used to resume a model session.
- **Run/turn**: one execution cycle processing a single input/event into outputs.

## Key Principles

1. **Thread id maps to session key**
   - RPC inputs always include a `thread_id`.
   - Holon resolves `thread_id` to a `session_key` and uses it consistently for:
     - queueing and concurrency
     - persistence and history
     - cross-session messaging

2. **Always have a default `main`**
   - On `holon serve` startup, ensure `main` exists as both:
     - `thread_id = "main"`
     - `session_key = "main"`
   - `main` acts as:
     - the interactive default for users (TUI)
     - an "inbox"/audit log for background work (events, cron-like triggers)

3. **Events route to stable partitions**
   - Events should not all pile into `main`, but they also should not fragment into per-event sessions by default.
   - Route events to a stable **partition key** derived from generic envelope fields, not GitHub-specific assumptions.

4. **Isolation and visibility are different**
   - Execution happens in an isolated session key.
   - A concise summary ("announce") is posted to `main` so users can see what happened.

## Proposed Routing

### RPC/TUI input

Inputs arrive via control plane `turn/start` (or equivalent) and include `thread_id`.

Default mapping:

```
session_key = thread_id.trim()
```

Rules:
- Empty/invalid `thread_id` is rejected.
- If `thread_id` is omitted by clients, server should default it to `main` (contract upgrade).

### Background event routing

Each event has an envelope with (at least) `source`, `type`, `scope`, `subject`.

Proposed default mapping:

1. If event includes a `thread_id` (explicit route): use it.
2. Else compute `event_partition_key`:
   - Prefer `scope.partition` (future-proof explicit field).
   - Else prefer `scope.repo` if present (GitHub happens to set it; still just a string).
   - Else fall back to `source + ":" + subject.kind + ":" + subject.id` when `subject` is present.
   - Else fall back to `source + ":" + type`.

Then:

```
session_key = "event:" + sanitize(event_partition_key)
thread_id   = session_key (or a display alias if UI wants)
```

Rationale:
- Same repository's events naturally share context without forcing "GitHub repo" as a first-class concept.
- Other event sources can pick their own stable `scope.partition` without changing server code.

## Concurrency Model

Borrow OpenClaw's proven approach:

- Serialize runs **per session key** (lane `session:<key>`).
- Apply a global concurrency cap across all lanes (e.g. `--max-concurrent` or config).
- Never run two turns concurrently against the same session key (prevents history races and tool collisions).

Queue modes are optional for v1, but should be designed for:
- `collect` (default): coalesce multiple queued messages into one follow-up
- `followup`: queue next turn after current completes
- `steer`: inject into current run at safe boundaries (best-effort)
- `interrupt`: abort current run and run newest (avoid unless explicitly requested)

## Notifications to `main` (Inbox / Announce)

We need a server-supported way for background work to remain visible without polluting `main` with raw event payloads.

### `announce` payload shape

Controller/event processing should emit a concise announce record:

```json
{
  "level": "info",
  "title": "GitHub event processed",
  "text": "Opened PR #123 and requested review from @foo.",
  "links": [
    { "label": "PR", "url": "https://github.com/org/repo/pull/123" }
  ],
  "source_session_key": "event:holon-run/holon",
  "event_id": "evt_...",
  "created_at": "2026-02-15T00:00:00Z"
}
```

### Delivery semantics

- The announce record is appended to `main` as a normal message/event in the server-side store.
- TUI shows the inbox stream on the `main` thread.
- The announce record should be small and stable; long logs go to artifacts/state files.

Whether `announce` becomes part of the LLM context for `main` is a policy choice:
- Recommended default: yes, but keep it short; optionally mark as "system notice" and apply aggressive truncation.

## Cross-Session Messaging Tools

We should provide tools (available to controllers/skills) to coordinate across sessions, similar to OpenClaw:

- `sessions_list`: list session keys and basic metadata (updated_at, status, recent messages count)
- `sessions_history`: fetch transcript for a session key (bounded, with optional tool filtering)
- `sessions_send`: send a message into another session key
  - `wait_seconds = 0` for fire-and-forget
  - `wait_seconds > 0` waits for a terminal result (best-effort)
- `sessions_spawn` (optional): start an isolated sub-run and announce back to requester

Security considerations:
- In sandboxed modes, restrict session visibility to only sessions created/spawned by the caller unless explicitly allowed.

## Storage (Server-Side)

Suggested files under `agent_home/state/` (exact layout can evolve):

- `state/sessions/index.json`: map `session_key -> { engine_session_id, updated_at, ... }`
- `state/sessions/<session_key>.jsonl`: transcript / messages
- `state/inbox/main.jsonl`: `announce` stream (could just be `sessions/main.jsonl`)

## Implementation Steps (Suggested)

1. Add explicit `session_key` to serve control plane and internal turn dispatch.
2. Enforce per-session serialization + global concurrency cap.
3. Implement announce-to-main store append and TUI display.
4. Add session tools (`sessions_list/history/send`) with safe defaults.
5. Add event routing via `scope.partition` / `scope.repo` fallback.

## Related Docs

- `docs/serve-notifications.md`: notification stream contract and `thread_id` usage.
- `docs/agent-service.md`: agent service control plane overview.

