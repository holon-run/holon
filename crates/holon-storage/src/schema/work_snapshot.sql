CREATE TABLE work_domain_captures (
  seed_id TEXT NOT NULL REFERENCES episode_continuity_seeds(seed_id),
  capture_phase TEXT NOT NULL CHECK (capture_phase IN ('upgrade','first_import','legacy_import')),
  metadata_digest TEXT NOT NULL,
  PRIMARY KEY(seed_id,capture_phase)
);
CREATE TABLE work_domain_eligibility (
  seed_id TEXT NOT NULL REFERENCES episode_continuity_seeds(seed_id),
  capture_phase TEXT NOT NULL,
  family TEXT NOT NULL CHECK (family IN ('work','wait','frame')),
  record_id TEXT NOT NULL,
  content_digest TEXT NOT NULL,
  PRIMARY KEY(seed_id,capture_phase,family,record_id),
  FOREIGN KEY(seed_id,capture_phase) REFERENCES work_domain_captures(seed_id,capture_phase) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE work_domain_current (
  agent_id TEXT NOT NULL,
  identity_incarnation INTEGER NOT NULL CHECK (identity_incarnation > 0),
  revision INTEGER NOT NULL CHECK (revision > 0),
  coverage_json TEXT NOT NULL,
  PRIMARY KEY(agent_id,identity_incarnation)
);
CREATE TABLE work_domain_records (
  agent_id TEXT NOT NULL,
  identity_incarnation INTEGER NOT NULL CHECK (identity_incarnation > 0),
  family TEXT NOT NULL CHECK (family IN ('work','wait','frame')),
  record_id TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK (revision > 0),
  availability TEXT NOT NULL CHECK (availability IN ('present','absent','pruned')),
  content_digest TEXT,
  fingerprint_json TEXT,
  CHECK ((availability='present' AND content_digest IS NOT NULL AND fingerprint_json IS NOT NULL)
      OR (availability!='present' AND content_digest IS NULL AND fingerprint_json IS NULL)),
  PRIMARY KEY(agent_id,identity_incarnation,family,record_id)
);
CREATE INDEX idx_work_domain_records_live
  ON work_domain_records(agent_id,identity_incarnation,family,availability);
CREATE INDEX idx_work_domain_eligibility_member
  ON work_domain_eligibility(seed_id,family,record_id);

CREATE TABLE work_domain_result_heads (
  agent_id TEXT NOT NULL,
  identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation > 0),
  work_item_id TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision > 0),
  head_json TEXT NOT NULL,
  PRIMARY KEY(agent_id,identity_incarnation,work_item_id)
);

-- Finite current-qualified Wait relations survive resident window trimming.
CREATE TABLE work_domain_wait_edges (
  agent_id TEXT NOT NULL,
  identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation > 0),
  edge_kind TEXT NOT NULL CHECK(edge_kind IN ('turn','task','trigger')),
  source_id TEXT NOT NULL,
  wait_id TEXT NOT NULL,
  availability TEXT NOT NULL CHECK(availability IN ('present','pruned')),
  revision INTEGER NOT NULL CHECK(revision > 0),
  PRIMARY KEY(agent_id,identity_incarnation,edge_kind,source_id,wait_id)
);
CREATE INDEX idx_work_domain_wait_edges_wait
  ON work_domain_wait_edges(agent_id,identity_incarnation,wait_id);

-- The exclusive preactivation actor can advance only a fixed unbound Wait.
-- Membership and the original head/window manifest stay immutable.
CREATE TABLE work_domain_seed_recovery_current (
  seed_id TEXT NOT NULL REFERENCES episode_continuity_seeds(seed_id),
  family TEXT NOT NULL CHECK(family='wait'),
  record_id TEXT NOT NULL,
  original_digest TEXT NOT NULL,
  content_digest TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision > 0),
  PRIMARY KEY(seed_id,family,record_id)
);
