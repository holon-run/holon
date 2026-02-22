# Serve Main Session Announce Visibility Plan

## Background

In `holon serve`, background event sessions (for example `event:<repo>`) already enqueue a summary event to `main` as `session.announce`.

Current issue:
- TUI connected via `/rpc/stream` does not reliably surface these summaries as user-visible timeline items.
- Operators must inspect state files (`events.ndjson` / controller jsonl) to understand what happened.

This document defines a minimal implementation plan to make actionable background outcomes visible in TUI.

## Scope

In scope:
- Show actionable background event outcomes in `main` thread UI stream.
- Preserve existing event pipeline (`session.announce` generation remains).
- No history replay on new stream connections.

Out of scope:
- Backfill/replay old announce events.
- Redesign of control-plane protocol beyond current notification families.

## Requirements

### R1. No Replay

Do not replay historical announce events when a new TUI client connects to `/rpc/stream`.
Only announcements produced after subscription should be pushed.

### R2. Show Event + Action

Displayed content must include both:
- Event identity/context (at least `event_id`, `source`, `type`, `source_session_key`)
- Adopted operation (decision/action)

### R3. Hide No-op

If the decision is `no_op`/`no-op` (or equivalent), do not emit UI-visible announce notification.

## Proposed Data Contract

Keep `session.announce` as ingress envelope, but enrich payload to make filtering/display deterministic.

Recommended payload fields:
- `event_id` (string, required)
- `source` (string, required)
- `type` (string, required)
- `source_session_key` (string, required)
- `title` (string, optional)
- `text` (string, optional)
- `decision` (string, required): one of `issue-solve`, `pr-review`, `pr-fix`, `no-op`, `unknown`
- `action` (string, optional): concrete operation summary (for example `opened_pr`, `posted_review`, `updated_branch`, `commented`)
- `created_at` (RFC3339 string, required)

Compatibility rule:
- If old payload has no `decision`, treat as `unknown` (display allowed).

## Runtime Behavior

### 1) Produce structured announce payload in serve handler

File: `cmd/holon/serve.go`

When building `session.announce` payload (`enqueueMainAnnounce`):
- Keep existing fields.
- Add `decision` and `action` derived from controller result.

Notes:
- Existing `result.Message` can remain human-readable text.
- New fields should be machine-filterable and stable.

### 2) Bridge announce to stream-visible item notifications

Files: `pkg/serve/webhook.go`, `pkg/serve/control.go`

After successful `HandleEvent` for `session.announce`:
- Parse announce payload.
- If `decision` is `no-op`/`no_op`, skip.
- Otherwise emit a system `item/created` notification into `main` thread via runtime broadcaster.

Suggested item content shape:
- `type: "system_announce"`
- `event_id`
- `source`
- `event_type` (mapped from payload `type`)
- `source_session_key`
- `decision`
- `action`
- `text`
- `created_at`

This keeps protocol changes minimal because TUI already consumes `item/created`.

### 3) TUI rendering

No protocol change required.

TUI should render `item/created` with `content.type == "system_announce"` as a concise background-event card.

## Filtering Rules

Announce emission to TUI must follow:
1. payload parse failure: do not emit (log warn)
2. missing essential event fields: do not emit (log warn)
3. decision in `{no-op, no_op}` (case-insensitive): do not emit
4. all others: emit once

## Observability

- Continue writing original `events.ndjson` / `decisions.ndjson` / `actions.ndjson` unchanged.
- Add debug log for emitted announce item with `event_id`, `decision`, `action`.
- Add debug log for skipped no-op announce with `event_id`.

## Test Plan

### Unit

1. `session.announce` with `decision=no-op` does not produce item notification.
2. `session.announce` with `decision=pr-fix` produces one `item/created` notification.
3. Missing/invalid payload fields are skipped safely.

### Integration

1. Start webhook server + stream subscriber.
2. Inject non-main event that results in announce (`decision=issue-solve`).
3. Assert stream receives `item/created` with `content.type=system_announce` and expected event/action fields.
4. Repeat with `decision=no-op`; assert no announce item arrives.

## Rollout

Phase 1 (this change):
- Structured announce payload
- Runtime bridge to `item/created`
- no-op filtering
- tests

Phase 2 (optional, not in this plan):
- replay/buffer support for late subscribers

## Acceptance Criteria

- TUI connected to `main` sees actionable background event summaries in real time.
- Displayed summary includes both event metadata and adopted operation.
- no-op outcomes are not shown in TUI.
- No historical replay behavior introduced.
