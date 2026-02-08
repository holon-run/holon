# GitHub-first MVP for `holon serve`

This document describes the current local-first MVP flow for running a proactive controller with GitHub events.

## Goal

Feed GitHub events into one long-running controller-agent session and let the controller skill decide follow-up actions.

## Run serve locally

```bash
holon serve \
  --repo holon-run/holon \
  --state-dir .holon/serve-state \
  --input -
```

`holon serve` reads one JSON object per line from stdin.

## Supported GitHub event inputs

The input line can be:

1. A normalized `EventEnvelope` JSON.
2. A raw GitHub webhook payload augmented with event metadata.

For raw payloads, event type can be provided by:

- top-level `event`
- top-level `x_github_event`
- `headers["X-GitHub-Event"]`

Delivery ID (for strong dedupe) can be provided by:

- top-level `x_github_delivery`
- `headers["X-GitHub-Delivery"]`

## Minimal examples

Issue comment:

```json
{"event":"issue_comment","action":"created","repository":{"full_name":"holon-run/holon"},"issue":{"number":527},"comment":{"id":101,"body":"@holonbot solve this"}}
```

PR review comment:

```json
{"event":"pull_request_review_comment","action":"created","repository":{"full_name":"holon-run/holon"},"pull_request":{"number":579},"comment":{"id":202,"body":"please fix this"}}
```

Forward test:

```bash
printf '%s\n' \
  '{"event":"issue_comment","action":"created","repository":{"full_name":"holon-run/holon"},"issue":{"number":527},"comment":{"id":101}}' \
  '{"event":"issue_comment","action":"created","repository":{"full_name":"holon-run/holon"},"issue":{"number":527},"comment":{"id":102}}' \
  | holon serve --repo holon-run/holon --state-dir .holon/serve-state --input -
```

## Runtime logs and state

Under `--state-dir`:

- `events.ndjson`: ingested normalized events
- `decisions.ndjson`: runtime dedupe/forward decisions
- `actions.ndjson`: handler outcomes
- `serve-state.json`: dedupe and cursor state
- `controller-state/`: controller channel, cursor, session metadata
- `controller-runtime/output/`: controller run outputs (including `execution.log`)

## Dedupe behavior (MVP)

- Preferred dedupe key source: GitHub delivery ID (`X-GitHub-Delivery`).
- Fallback dedupe keys are event-specific (comment/review IDs, etc.).
- Label-change events are normalized to reduce duplicate-trigger noise from multiple GitHub event sources.

