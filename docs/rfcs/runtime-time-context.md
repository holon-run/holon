# Runtime Time Context (#3410)

## Contract

Before each new provider inference, the runtime samples its injected clock and
renders a small runtime-owned time block with two fields:

- `current_time`: local RFC3339 timestamp with seconds and UTC offset.
- `timezone`: the validated IANA timezone name.

One UTC instant and one parsed timezone produce both fields. The timezone is
selected once per turn: agent override, runtime default, then UTC. Explicit
invalid timezone configuration is rejected; host locale, IP, language, message
payloads, and old reports do not supply the authoritative timezone or clock.

The block is a sampling snapshot, not a continuously updating clock. The newest
runtime sample takes precedence over earlier time blocks. Message creation time
belongs to its message and is not the time of execution or the event occurrence.
Report dates and reporting windows remain task-specific, not universal fields.

## Placement, replay, and cache

Time is appended after the materialized input and, on tool continuation, after
the previous assistant/tool results and any new input. It uses a `TurnScoped`
user content block, without a cache breakpoint. Runtime provenance does not
turn this user message into new system/developer authority.

The time sample is captured outside the request retry/reprojection loop.
Retries and pure budget reprojections reuse it. Each real provider round records
the text it received; replay places that text before its assistant response.
Later rounds append a new sample rather than editing the earlier sample.
Compaction may discard old rounds under its existing contract, but must retain
the complete current sample or fail the minimum request budget explicitly.
Initial context planning reserves a fixed 128 estimated tokens for the complete
time block, independent of its changing values. The remaining pinned context
must fit the existing planner or fail. This is not a new full-request admission
gate: it does not reject initial requests based on unrelated tool/image
estimates. Continuations account for their current sample explicitly.

Time does not enter the stable prompt frame, prompt cache key, or stable
fingerprint. Request diagnostics include the current sample separately.
Anthropic's rolling marker skips unmarked `TurnScoped` content blocks, so the
current sample remains after the reusable history boundary. Existing explicit
context breakpoints retain their behavior and the four-breakpoint limit.
OpenAI Chat/Responses preserve the appended conversation order.

This extends [Anthropic Turn-Scoped Cache Prefix](anthropic-turn-scoped-cache-prefix.md):
the ordinary turn context remains at its existing history head, while the small
time reminder is an append-only exception. Changing ordinary context or
compacting history can still invalidate a historical prefix. Structural tests
are evidence of placement and stable identity, not a claim about billed cache
hits or savings.

## Reporting boundary

This contract provides a clock, not a reporting scheduler or filesystem date
sandbox. A controlled report writer must derive its filename and half-open
window from a trusted schedule occurrence and business timezone, retain the
target on delayed/repeated wakes, and reject future normal reports. Historical
backfill needs explicit authorization; it does not authorize future reports.
Arbitrary shell/file writes are outside such a writer's guarantee.

No changes to generic timer, wake, or admission state machines are implied.
The daily-report end-to-end criterion remains open until an actual reporting
path and its business-day rule are selected and verified.

## Verification

Agent overrides can be managed through the existing control client:

```sh
holon agent timezone get
holon agent timezone set Asia/Shanghai
holon agent timezone clear
holon agent timezone set America/New_York agent-id
```

These commands are operator-only. Agent invocation context is rejected before
creating a control client, and malformed context must not fall back to operator
mode. The optional positional agent ID defaults to the configured default agent.
The authenticated control routes are
`GET/POST /api/control/agents/{agent_id}/timezone` and
`POST /api/control/agents/{agent_id}/timezone/clear`. Set accepts an IANA
`timezone` string and rejects invalid values with HTTP 400 before writing;
clear removes only that agent's override and restores runtime/UTC fallback.

Use fixed clock samples for UTC/local midnight, year changes, IANA offsets and
DST, plus invalid configuration and old-state compatibility. Verify consecutive
inferences preserve the previous wire prefix, retries reuse the same text,
current-time blocks survive compaction or produce an explicit budget error, and
both Anthropic strategies and OpenAI dialects retain time after input/results.
