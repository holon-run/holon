# 109: Agent roster snapshot reads one committed view

## Decision

`GET /api/agents/snapshot` (S4 of the observer-sync RFC) assembles every
roster anchor from one deferred SQLite read transaction on the runtime
database: identity registry, active public membership, per-Agent committed
event windows, latest canonical Briefs, and the runtime identity metadata
used to derive `visibility_scope_id`. The HTTP handler only authorizes,
gates on the durable `agents.roster-snapshot.v1` verification, enforces the
first-version hard limits (512 Agents, 4 MiB serialized, 10 s assembly
budget), and serializes.

## Reason

The RFC requires membership, event head/floor, and latest Brief to share one
committed view, and requires the response to be all-or-nothing. Chaining
`/agents/list`, the Brief APIs, and the event APIs (separate connections)
could mix commits and silently drop members. One transaction on the shared
runtime database is the smallest structure that satisfies the contract
without a second summary table.

## Preserved boundaries and tradeoffs

- Snapshot entries reflect committed canonical state, not in-memory runtime
  watchers; `/agents/list` keeps its live-runtime preference and its
  placeholder fallbacks for compatibility. Presentation facts of a loaded
  Agent may briefly lag behind its in-memory state until persistence.
- Unlike `/agents/list`, a per-Agent read failure fails the whole snapshot
  (500) instead of substituting a stopped placeholder: a partial roster
  would read as deletion to clients. A registered member with no committed
  state yet still gets the stopped placeholder, matching list semantics.
- `visibility_scope_id` is derived inside the view (runtime id + policy
  generation) plus the request's authority mode: local unauthenticated
  requests use the S1 public scope, control-token requests a distinct
  control scope. Credentials are never an input.
- The projection gate caches snapshot bytes under `AgentsRosterSnapshot`
  for its normal TTL; a cached response is still a committed view, and the
  capability check runs before the gate on every request.
- The durable `roster_snapshot_verified` check re-runs on open: unreadable
  membership payloads degrade the capability to unadvertised instead of
  failing startup.
