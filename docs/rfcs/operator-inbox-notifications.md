# Operator inbox notifications

The inbox is a control-plane projection. It does not alter conversation
unread/read markers and does not turn ordinary agent events into notifications.

- **Pending** (`GET /api/notifications/pending`) is the complete set of valid,
  actionable `WaitFor(operator_input)` records visible to the requester.
  Opening a conversation does not clear any entry. The runtime must resolve,
  cancel, replace, or invalidate the wait. A silent wait can be pending without
  creating a recent notification.
- **Recent** (`GET /api/notifications/recent`) lists committed notifications:
  an actionable wait with a formal brief, a WorkItem completion with a
  canonical result brief and a known recipient, or the first formal brief
  explicitly linked to an authenticated operator input. A single brief can
  notify each recipient only once; priority is wait, completion, then reply.
  The list contains metadata and links, not brief content. It returns up to
  100 entries from the last 30 days. Deduplication keys follow the lifetime
  of the canonical briefs, so aging off the list does not reset eligibility.
  Deleting an Agent clears its recent notifications and deduplication keys.
  Older notifications and WorkItem owners are not backfilled.
- **Refresh** uses `notifications.changed` with `{"v":1}` on the existing
  `/api/events/stream`. The frame has no ID, cursor, target, or count. A
  subscriber only receives it for a change they are authorized to see.
  A client reads authoritative lists on connect and reconnect; a recovered
  entry is not a new toast.

Both reads use control-plane authentication, return `Cache-Control: no-store`,
and sort newest first with a stable ID tiebreaker. Viewing a target or
admitting an unrelated prompt does not count as acting on a wait. The inbox
does not add an intervention endpoint or change the rules for matching input.
