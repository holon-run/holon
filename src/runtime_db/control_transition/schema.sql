-- REVIEW-ONLY D2 focused schema; not registered as a migration or activated.
-- Choose the next schema version only when integrated after D1.
CREATE TABLE IF NOT EXISTS control_command_receipts (
  agent_id TEXT NOT NULL,
  identity_incarnation INTEGER NOT NULL CHECK (identity_incarnation > 0),
  command_id TEXT NOT NULL CHECK (length(command_id) > 0),
  family_version INTEGER NOT NULL CHECK (family_version = 1),
  action TEXT NOT NULL CHECK (action IN ('start', 'stop')),
  invocation_digest TEXT NOT NULL,
  command_digest TEXT NOT NULL,
  committed_at TEXT NOT NULL,
  outcome_json TEXT NOT NULL,
  -- audit/index bindings and obligation IDs are immutable original results.
  receipt_json TEXT NOT NULL,
  PRIMARY KEY (agent_id, identity_incarnation, command_id)
);
-- No FK to current agent_identities: old incarnation replay fence survives
-- identity reconstruction. Supported result pruning needs a separate dedup fence.
CREATE TABLE IF NOT EXISTS control_cancellation_obligations (
  agent_id TEXT NOT NULL,
  identity_incarnation INTEGER NOT NULL CHECK (identity_incarnation > 0),
  command_id TEXT NOT NULL,
  obligation_id TEXT NOT NULL,
  target_kind TEXT NOT NULL CHECK (target_kind IN ('run', 'task', 'timer_loop', 'worktree')),
  target_identity TEXT NOT NULL,
  target_generation_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('pending', 'completed', 'needs_attention')),
  updated_at TEXT NOT NULL,
  -- Payload is exact intended target. Completion evidence/CAS never rewrites it.
  target_json TEXT NOT NULL,
  completion_evidence_json TEXT,
  PRIMARY KEY (agent_id, identity_incarnation, command_id, obligation_id),
  FOREIGN KEY (agent_id, identity_incarnation, command_id)
    REFERENCES control_command_receipts(agent_id, identity_incarnation, command_id),
  CHECK (state != 'completed' OR completion_evidence_json IS NOT NULL)
);
CREATE INDEX IF NOT EXISTS control_cancellation_pending_scope
  ON control_cancellation_obligations(agent_id, identity_incarnation, created_at, obligation_id)
  WHERE state IN ('pending', 'needs_attention');
-- Schema intentionally does not add a second Agent runnable/focus authority,
-- administrative revision counter, broad mutation journal, or receipt TTL.
