# Observer-sync identity foundations (S1)

Source: `docs/rfcs/observer-sync-agent-summary-and-read-markers.md` (S1
identity slice). S1 makes the runtime's identity facts durable and gates the
S0 capability evaluator on stored verification results instead of hard-coded
defaults.

Decisions:

- Runtime identity stays in the existing `runtime_metadata` key-value table
  (`runtime_id`, `event_log_epoch`,
  `visibility_policy_generation`) and is minted lazily with
  `INSERT OR IGNORE` at migration time. This matches the epoch lifecycle the
  event envelope already depends on: reopen preserves, a replaced database
  mints new values, and no epoch rotation API is added before a caller needs
  one.
- Agent identity reservations live in `agent_identity_reservations` and are
  deliberately not epoch-scoped: the existing deletion contract already
  promises that deleted agent ids remain reserved forever, so an epoch
  rotation never releases a reservation. Deletion moves a reservation to
  `retired`; the row is never removed. The guard runs inside
  `upsert_agent_identity_tx`, the single transaction-level registry write, so
  every creation, import, and replay path enforces it.
- The v48 backfill treats `agent_identities` as authoritative for current
  availability and only adds `retired` reservations from historical sources
  (`agent_states`, deletion jobs, audit scopes, the legacy `agents` table).
  It must stay idempotent under downgrade/re-upgrade cycles, so re-runs only
  add missing rows and converge tombstones. Name-accepted upgrade paths may
  lack historical tables entirely; each source is gated on table existence
  and a missing source keeps the capability unverified rather than failing
  the migration.
- Verification results are durable rows in
  `observer_sync_capability_verifications`, recomputed on every
  migration/open. Invariant violations record `verified = 0` (capability
  stays off) instead of failing the open; only structural database errors
  fail. The handshake loads these rows through the S0 evaluator and degrades
  to all-disabled on a load error, so a degraded database degrades
  advertisement rather than startup or discovery.

After S1 only `runtime_identity_stable` and `agent_identity_reserved` can
become true; the snapshot/event/brief verification families stay absent
(false) until their slices land, so no observer-sync capability is advertised
yet.
