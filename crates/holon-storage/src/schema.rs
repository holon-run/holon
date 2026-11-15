//! DDL shared by runtime migrations and storage tests.

pub const EPISODE_CONTINUITY_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS episode_continuity_seeds (
  seed_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  policy_version INTEGER NOT NULL CHECK (policy_version = 1),
  boundary_id TEXT NOT NULL,
  identity_incarnation INTEGER CHECK (identity_incarnation IS NULL OR identity_incarnation > 0),
  source_kind TEXT NOT NULL CHECK (source_kind IN (
    'upgrade_identity', 'upgrade_unbound', 'legacy_identity_import', 'legacy_state_import'
  )),
  identity_digest TEXT,
  roots_json TEXT NOT NULL,
  candidate_payloads_json TEXT NOT NULL,
  import_required INTEGER NOT NULL CHECK (import_required IN (0, 1)),
  captured_at TEXT NOT NULL,
  UNIQUE (agent_id, policy_version)
);
CREATE TABLE IF NOT EXISTS agent_episode_continuity (
  agent_id TEXT NOT NULL,
  identity_incarnation INTEGER NOT NULL CHECK (identity_incarnation > 0),
  policy_version INTEGER NOT NULL CHECK (policy_version = 1),
  mode TEXT NOT NULL CHECK (mode IN ('legacy_adopted', 'fresh_empty')),
  seed_id TEXT,
  bound_at TEXT NOT NULL,
  CHECK ((mode = 'fresh_empty' AND seed_id IS NULL)
      OR (mode = 'legacy_adopted' AND seed_id IS NOT NULL)),
  PRIMARY KEY (agent_id, identity_incarnation, policy_version),
  FOREIGN KEY (seed_id) REFERENCES episode_continuity_seeds(seed_id)
);
CREATE TABLE IF NOT EXISTS episode_continuity_members (
  seed_id TEXT NOT NULL,
  capture_phase TEXT NOT NULL CHECK (capture_phase IN ('upgrade', 'first_import')),
  episode_id TEXT NOT NULL,
  content_digest TEXT NOT NULL,
  qualification_json TEXT NOT NULL,
  PRIMARY KEY (seed_id, capture_phase, episode_id),
  FOREIGN KEY (seed_id) REFERENCES episode_continuity_seeds(seed_id)
);
CREATE INDEX IF NOT EXISTS idx_episode_continuity_member_episode
  ON episode_continuity_members(episode_id, seed_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_episode_continuity_seed_binding
  ON agent_episode_continuity(seed_id) WHERE seed_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS episode_continuity_imports (
  seed_id TEXT PRIMARY KEY,
  checkpoint_digest TEXT NOT NULL,
  roots_json TEXT NOT NULL,
  captured_at TEXT NOT NULL,
  FOREIGN KEY (seed_id) REFERENCES episode_continuity_seeds(seed_id)
);
"#;

pub const INPUT_EVIDENCE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS input_evidence_captures (
 seed_id TEXT NOT NULL, capture_phase TEXT NOT NULL, policy_version INTEGER NOT NULL CHECK(policy_version=1),
 includes_tails INTEGER NOT NULL CHECK(includes_tails IN (0,1)), metadata_digest TEXT NOT NULL,
 PRIMARY KEY(seed_id,capture_phase), FOREIGN KEY(seed_id) REFERENCES episode_continuity_seeds(seed_id)
);
CREATE TABLE IF NOT EXISTS input_evidence_eligibility (
 seed_id TEXT NOT NULL, capture_phase TEXT NOT NULL CHECK(capture_phase IN ('upgrade','first_import','legacy_import')),
 family TEXT NOT NULL, record_id TEXT NOT NULL, fingerprint_json TEXT NOT NULL,
 mandatory INTEGER NOT NULL CHECK(mandatory IN (0,1)),
 PRIMARY KEY(seed_id,capture_phase,family,record_id),
 FOREIGN KEY(seed_id) REFERENCES episode_continuity_seeds(seed_id)
);
CREATE TABLE IF NOT EXISTS input_evidence_execution_seed_sources (
 seed_id TEXT NOT NULL, capture_phase TEXT NOT NULL, source_message_id TEXT NOT NULL,
 metadata_json TEXT NOT NULL, content_digest TEXT NOT NULL,
 PRIMARY KEY(seed_id,source_message_id), FOREIGN KEY(seed_id) REFERENCES episode_continuity_seeds(seed_id)
);
CREATE TABLE IF NOT EXISTS input_evidence_execution_seed_source_recovery (
 seed_id TEXT NOT NULL, source_message_id TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0),
 metadata_json TEXT NOT NULL, content_digest TEXT NOT NULL,
 PRIMARY KEY(seed_id,source_message_id), FOREIGN KEY(seed_id) REFERENCES episode_continuity_seeds(seed_id)
);
CREATE TABLE IF NOT EXISTS input_evidence_execution_current_sources (
 agent_id TEXT NOT NULL, identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 source_message_id TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0),
 metadata_json TEXT NOT NULL, content_digest TEXT NOT NULL,
 PRIMARY KEY(agent_id,identity_incarnation,source_message_id)
);
CREATE TABLE IF NOT EXISTS input_evidence_execution_seed_work (
 seed_id TEXT NOT NULL REFERENCES episode_continuity_seeds(seed_id), work_item_id TEXT NOT NULL,
 capture_phase TEXT NOT NULL, presence TEXT NOT NULL CHECK(presence IN ('present','absent')),
 PRIMARY KEY(seed_id,work_item_id));
CREATE TABLE IF NOT EXISTS input_evidence_execution_current_work (
 agent_id TEXT NOT NULL, identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0), work_item_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0), presence TEXT NOT NULL CHECK(presence IN ('present','absent','pruned')),
 PRIMARY KEY(agent_id,identity_incarnation,work_item_id));
CREATE INDEX IF NOT EXISTS idx_execution_protocol_source_message
 ON execution_protocol_attempts(agent_id,json_extract(payload_json,'$.source_message_id'));
CREATE TABLE IF NOT EXISTS input_evidence_seed_attempt_turns (
 seed_id TEXT NOT NULL, capture_phase TEXT NOT NULL, attempt_id TEXT NOT NULL,
 proof_json TEXT NOT NULL, PRIMARY KEY(seed_id,capture_phase,attempt_id),
 FOREIGN KEY(seed_id) REFERENCES episode_continuity_seeds(seed_id)
);
CREATE INDEX IF NOT EXISTS idx_input_evidence_eligibility_lookup
 ON input_evidence_eligibility(seed_id,family,record_id);
CREATE TABLE IF NOT EXISTS input_evidence_current (
 agent_id TEXT NOT NULL, identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 family TEXT NOT NULL, record_id TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0),
 content_digest TEXT, fingerprint_json TEXT NOT NULL, attempt_turn_proof_json TEXT,
 availability TEXT NOT NULL CHECK(availability IN ('present','pruned')),
 qualification TEXT NOT NULL CHECK(qualification IN ('legacy_prepared','committed_writer')),
 seed_id TEXT, PRIMARY KEY(agent_id,identity_incarnation,family,record_id),
 FOREIGN KEY(seed_id) REFERENCES episode_continuity_seeds(seed_id)
);
CREATE TABLE IF NOT EXISTS input_evidence_seed_recovery_current (
 seed_id TEXT NOT NULL, family TEXT NOT NULL, record_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0), fingerprint_json TEXT NOT NULL,
 origin_kind TEXT NOT NULL CHECK(origin_kind IN ('fixed_member','recovery_created','maintenance_created')),
 origin_attempt_id TEXT, command_kind TEXT, command_identity TEXT, command_hash TEXT, maintenance_proof_json TEXT,
 PRIMARY KEY(seed_id,family,record_id), FOREIGN KEY(seed_id) REFERENCES episode_continuity_seeds(seed_id),
 CHECK ((origin_kind='fixed_member' AND origin_attempt_id IS NULL AND command_kind IS NULL AND command_identity IS NULL AND command_hash IS NULL AND maintenance_proof_json IS NULL)
     OR (origin_kind='recovery_created' AND origin_attempt_id IS NOT NULL AND command_kind IS NOT NULL AND command_identity IS NOT NULL AND command_hash IS NOT NULL AND maintenance_proof_json IS NULL)
     OR (origin_kind='maintenance_created' AND origin_attempt_id IS NULL AND command_kind IS NULL AND command_identity IS NULL AND command_hash IS NULL AND maintenance_proof_json IS NOT NULL))
);
CREATE VIEW IF NOT EXISTS input_evidence_seed_membership AS
 SELECT e.seed_id,e.capture_phase,e.family,e.record_id,e.fingerprint_json,e.mandatory
 FROM input_evidence_eligibility e WHERE NOT EXISTS(
   SELECT 1 FROM input_evidence_seed_recovery_current r WHERE r.seed_id=e.seed_id AND r.family=e.family AND r.record_id=e.record_id)
 UNION ALL
 SELECT r.seed_id,'startup_recovery',r.family,r.record_id,r.fingerprint_json,1
 FROM input_evidence_seed_recovery_current r;
"#;

pub const INPUT_MUTATION_RECEIPT_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS input_mutation_receipts (
  command_nonce TEXT PRIMARY KEY CHECK(length(command_nonce) > 0),
  family TEXT NOT NULL,
  phase_version INTEGER NOT NULL CHECK(phase_version = 1),
  command_digest TEXT NOT NULL CHECK(length(command_digest) = 64),
  scope_digest TEXT NOT NULL CHECK(length(scope_digest) = 64),
  scopes_json TEXT NOT NULL,
  prepared_at TEXT NOT NULL,
  prepared_json TEXT NOT NULL,
  outcome_json TEXT NOT NULL,
  committed_at TEXT NOT NULL,
  result_digest TEXT NOT NULL CHECK(length(result_digest) = 64)
);
"#;

pub const WORK_DOMAIN_SCHEMA: &str = include_str!("schema/work_snapshot.sql");

pub const DISPATCH_MEMORY_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS dispatch_memory_checkpoints (
  agent_id TEXT NOT NULL,
  identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation > 0),
  turn_id TEXT NOT NULL,
  source_message_id TEXT NOT NULL,
  turn_index INTEGER NOT NULL CHECK(turn_index > 0),
  state TEXT NOT NULL CHECK(state IN ('started', 'pending', 'done')),
  seed_digest TEXT NOT NULL CHECK(length(seed_digest) = 64),
  record_digest TEXT NOT NULL CHECK(length(record_digest) = 64),
  payload_json TEXT NOT NULL,
  PRIMARY KEY(agent_id, identity_incarnation, turn_id)
);
CREATE TABLE IF NOT EXISTS dispatch_memory_legacy_dispatches (
  agent_id TEXT NOT NULL,
  identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation > 0),
  turn_id TEXT NOT NULL,
  source_message_id TEXT NOT NULL,
  turn_index INTEGER NOT NULL CHECK(turn_index > 0),
  PRIMARY KEY(agent_id, identity_incarnation, turn_id)
);
CREATE TABLE IF NOT EXISTS dispatch_memory_retired_handoffs (
  agent_id TEXT NOT NULL,
  identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation > 0),
  turn_id TEXT NOT NULL,
  source_message_id TEXT NOT NULL,
  turn_index INTEGER NOT NULL CHECK(turn_index > 0),
  seed_digest TEXT NOT NULL CHECK(length(seed_digest) = 64),
  checkpoint_digest TEXT NOT NULL CHECK(length(checkpoint_digest) = 64),
  control_command_id TEXT NOT NULL,
  control_digest TEXT NOT NULL CHECK(length(control_digest) = 64),
  run_id TEXT NOT NULL,
  runtime_lease_id TEXT NOT NULL,
  record_digest TEXT NOT NULL CHECK(length(record_digest) = 64),
  payload_json TEXT NOT NULL,
  PRIMARY KEY(agent_id, identity_incarnation, turn_id)
);
CREATE TABLE IF NOT EXISTS dispatch_memory_auto_commands (
  command_nonce TEXT PRIMARY KEY CHECK(length(command_nonce) > 0),
  base_command_digest TEXT NOT NULL CHECK(length(base_command_digest) = 64),
  final_command_digest TEXT NOT NULL CHECK(length(final_command_digest) = 64),
  metadata_digest TEXT NOT NULL CHECK(length(metadata_digest) = 64),
  payload_json TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_dispatch_memory_one_unfinished
  ON dispatch_memory_checkpoints(agent_id, identity_incarnation) WHERE state != 'done';
CREATE INDEX IF NOT EXISTS idx_dispatch_memory_unfinished
  ON dispatch_memory_checkpoints(agent_id, identity_incarnation, turn_index, turn_id)
  WHERE state != 'done';
"#;

pub const TASK_DOMAIN_VERSION: i64 = 100;

pub const TASK_DOMAIN_SCHEMA: &str = include_str!("schema/task_snapshot.sql");

pub const SCHEDULING_SCHEMA: &str = include_str!("schema/scheduling_snapshot.sql");

pub const CURRENT_RUN_ABORT_SCHEMA: &str = include_str!("schema/current_run_abort.sql");
pub const CURRENT_RUN_ABORT_HISTORY_INDEX_SCHEMA: &str =
    include_str!("schema/current_run_abort_history_index.sql");

pub const INPUT_NATIVE_BIRTH_VERSION: i64 = 102;

pub const INPUT_NATIVE_BIRTH_SCHEMA: &str = r#"
CREATE TABLE input_query_birth_cursors (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(typeof(identity_incarnation)='integer' AND identity_incarnation>0),
 high_water INTEGER NOT NULL CHECK(typeof(high_water)='integer' AND high_water>=0),
 PRIMARY KEY(agent_id,identity_incarnation));
CREATE TABLE input_query_birth_members (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(typeof(identity_incarnation)='integer' AND identity_incarnation>0),
 family TEXT NOT NULL CHECK(family IN ('message','turn','tool_execution','brief','transcript','attempt','outcome','work_execution')),
 record_id TEXT NOT NULL CHECK(length(CAST(record_id AS BLOB))>0),
 birth_seq INTEGER NOT NULL CHECK(typeof(birth_seq)='integer' AND birth_seq>=0),
 PRIMARY KEY(agent_id,identity_incarnation,family,record_id),
 FOREIGN KEY(agent_id,identity_incarnation) REFERENCES input_query_birth_cursors(agent_id,identity_incarnation));
CREATE INDEX input_native_known_current ON input_evidence_current(family,record_id);
CREATE INDEX input_native_known_fixed ON input_evidence_eligibility(family,record_id);
CREATE INDEX input_native_known_recovery ON input_evidence_seed_recovery_current(family,record_id);
CREATE INDEX input_native_known_birth ON input_query_birth_members(family,record_id);
CREATE INDEX input_query_birth_frontier ON input_query_birth_members(agent_id,identity_incarnation,birth_seq,family,record_id);
CREATE UNIQUE INDEX input_query_birth_unique ON input_query_birth_members(agent_id,identity_incarnation,birth_seq) WHERE birth_seq>0;
"#;

pub const CURRENT_READ_WRITES_VERSION: i64 = 103;

pub const CURRENT_READ_WRITES_SCHEMA: &str = r#"
CREATE TABLE current_read_revisions (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 revision INTEGER NOT NULL CHECK(revision>=0),
 PRIMARY KEY(agent_id,identity_incarnation));
CREATE TABLE current_read_qualification (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 family TEXT NOT NULL CHECK(family IN ('queue','event')),
 record_id TEXT NOT NULL CHECK(length(CAST(record_id AS BLOB))>0),
 revision INTEGER NOT NULL CHECK(revision>0),
 qualification_state TEXT NOT NULL CHECK(qualification_state IN ('qualified','unborn','absent','unprepared')),
 content_digest TEXT,
 CHECK((qualification_state='qualified' AND content_digest IS NOT NULL AND length(content_digest)=64)
    OR (qualification_state IN ('unborn','absent','unprepared') AND content_digest IS NULL)),
 PRIMARY KEY(agent_id,identity_incarnation,family,record_id),
 FOREIGN KEY(agent_id,identity_incarnation) REFERENCES current_read_revisions(agent_id,identity_incarnation));
CREATE INDEX current_read_known_address ON current_read_qualification(family,record_id);
CREATE TABLE event_history_acks (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 audit_event_id TEXT NOT NULL CHECK(length(CAST(audit_event_id AS BLOB))>0),
 event_seq INTEGER NOT NULL CHECK(event_seq>0),
 content_digest TEXT NOT NULL CHECK(length(content_digest)=64),
 PRIMARY KEY(agent_id,identity_incarnation,audit_event_id));
CREATE INDEX event_history_ack_address ON event_history_acks(audit_event_id);
"#;

pub const COMMAND_TASK_CREATION_VERSION: i64 = 104;

pub const COMMAND_TASK_CREATION_SCHEMA: &str = include_str!("schema/command_task_creation.sql");

pub const TASK_STOP_SCHEMA: &str = include_str!("schema/task_stop.sql");

pub const CONTROL_TRANSITION_SCHEMA: &str = include_str!("schema/control_transition.sql");

// Normal single-agent receipts can be looked up without decoding the receipt
// ledger. Other shapes still use the original membership and ownership proof.
#[doc(hidden)]
#[macro_export]
macro_rules! single_receipt_scope_agent {
    () => {
        "CASE WHEN json_valid(scopes_json)=0 THEN NULL
          WHEN json_type(scopes_json)='array'
          AND json_array_length(scopes_json)=1
          AND json_type(scopes_json,'$[0]')='object'
          THEN json_extract(scopes_json,'$[0].agent_id') END"
    };
}

pub const RECEIPT_SCOPE_INDEX_SCHEMA: &str = concat!(
    "CREATE INDEX IF NOT EXISTS idx_input_mutation_receipt_single_agent
     ON input_mutation_receipts ((",
    single_receipt_scope_agent!(),
    "));"
);
