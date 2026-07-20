# Scheduler Protocol Persistence Uses Normalized Canonical Facts

## Choice

Extend the existing `runtime_db` transaction domain with normalized scheduler
protocol tables. Do not persist the pure reducer `Snapshot` as authority, and
do not create a separately mutable `WorkDispatchIntent` table in Phase 2.

Accepted dispatch intent becomes a fenced `scheduler_work_demands` transition
plus audit evidence. Activation, settlement, authority, wait generation, agent
slot, dispatch revision, and durable focus remain separately queryable
canonical facts. Every agent-local fact carries `agent_id` through its primary
and foreign keys so one reducer snapshot can be rebuilt without consulting or
scanning another agent's rows.

Persist immutable command-result and migration-result ledgers alongside the
protocol facts. They retain versioned payload hashes and successful or rejected
canonical outcomes, so restart returns a first-seen result instead of
re-evaluating a stale command against newer state. Hashing occurs only after
the declared wire version has been decoded into a validated typed command and
accepted aliases and defaults have been normalized.

## Reason

The runtime database already provides SQLite transactions, revision checks,
fault rollback, audit writes, and durable outboxes. Reusing that boundary keeps
legacy compatibility writes and new protocol facts atomic. A second mutable
intent representation or authoritative snapshot blob would require
reconciliation and could let a projection disagree with admission authority.
Without explicit agent partitioning, canonical focus, and command outcomes,
the normalized facts would still depend on legacy projections and could not
provide deterministic restart or replay.

## Preserved Boundary

The pure protocol kernel remains storage-independent. Database constraints
reinforce its invariants but do not replace reducer validation. Scheduler read
models, WorkItem readiness, and `AgentState.status` remain rebuildable
projections. `agent_states.current_work_item_id` remains authoritative during
legacy and shadow operation, is atomically compared and dual-written during
cutover, and becomes a compatibility projection only after the normalized
focus row is authoritative. Production authority does not change during
additive persistence work. Any optional serialized snapshot is a discardable
versioned cache with strict field-presence semantics; omitted required fields
must not be interpreted as explicit null values through legacy serde defaults.
