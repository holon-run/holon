# Agent id reincarnation as same-row replacement with an incarnation counter

## Decision

Re-creating a fully deleted agent id replaces the existing
`agent_identities` row (same primary key) with a fresh Active record whose
`incarnation` counter increments monotonically; there is no incarnation
suffix in the agent id and no separate per-incarnation history table.

## Why

- Every FK in the runtime DB references `agent_identities.agent_id`.
  Replacing the row keeps those references valid without a migration, while
  a new id (or an incarnation-keyed table) would require rewiring every
  referencing table and reader.
- The identity reservation guard (`retired` ids are never implicitly
  reused) stays intact: the only `retired -> active` transition happens
  inside the single `reincarnate_with_bootstrap_and_relations` transaction,
  after the latest deletion job reached `Completed`. The observer-sync
  reservation probe and integrity checks are unchanged.
- The old and new incarnations stay distinguishable through the monotonic
  `incarnation` counter plus the `agent_recreated` audit boundary event;
  deletion jobs and audit history are retained as-is.

## Preserved boundary / tradeoff

Per-agent live projections (`agent_states`, `agent_bootstraps`,
`agent_lineages`, `agent_supervisions`) are cleared inside the same
transaction so nothing leaks into the new incarnation. External stale
references (notes, allowlists, other agents' memories) resolve to the new
incarnation; this is accepted and mitigated only by the incarnation counter
and audit boundary, not by blocking reuse.
