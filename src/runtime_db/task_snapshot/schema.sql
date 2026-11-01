CREATE TABLE task_domain_captures (
 seed_id TEXT NOT NULL REFERENCES episode_continuity_seeds(seed_id),
 capture_phase TEXT NOT NULL CHECK(capture_phase IN ('upgrade','legacy_import','first_import')),
 metadata_digest TEXT NOT NULL,
 PRIMARY KEY(seed_id,capture_phase)
);
CREATE TABLE task_domain_eligibility (
 seed_id TEXT NOT NULL,
 capture_phase TEXT NOT NULL,
 family TEXT NOT NULL CHECK(family IN ('task','result_settlement')),
 record_id TEXT NOT NULL,
 availability TEXT NOT NULL CHECK(availability IN ('present','unavailable')),
 content_digest TEXT,
 projection_digest TEXT,
 CHECK((availability='present' AND content_digest IS NOT NULL AND projection_digest IS NOT NULL) OR (availability='unavailable' AND content_digest IS NULL AND projection_digest IS NULL)),
 PRIMARY KEY(seed_id,capture_phase,family,record_id),
 FOREIGN KEY(seed_id,capture_phase) REFERENCES task_domain_captures(seed_id,capture_phase) DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_task_domain_eligibility_id ON task_domain_eligibility(family,record_id);
CREATE TABLE task_domain_current (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 revision INTEGER NOT NULL CHECK(revision>0),
 coverage_json TEXT NOT NULL,
 PRIMARY KEY(agent_id,identity_incarnation)
);
CREATE TABLE task_domain_records (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 family TEXT NOT NULL CHECK(family IN ('task','result_settlement')),
 record_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 availability TEXT NOT NULL CHECK(availability IN ('present','absent','pruned')),
 content_digest TEXT,
 projection_digest TEXT,
 CHECK((availability='present' AND content_digest IS NOT NULL AND projection_digest IS NOT NULL)
    OR (availability!='present' AND content_digest IS NULL AND projection_digest IS NULL)),
 PRIMARY KEY(agent_id,identity_incarnation,family,record_id)
);
CREATE INDEX idx_task_domain_records_scope ON task_domain_records(agent_id,identity_incarnation,family,availability);
-- Captured in the original JSON Task import transaction, never by a later reader.
CREATE TABLE task_domain_import_inventory (
 agent_id TEXT NOT NULL,
 task_id TEXT NOT NULL,
 received_digest TEXT NOT NULL,
 stored_digest TEXT NOT NULL,
 stored_projection_digest TEXT NOT NULL,
 actual_changed INTEGER NOT NULL CHECK(actual_changed IN (0,1)),
 PRIMARY KEY(agent_id,task_id)
);

-- Exclusive original unbound recovery advances versions, not fixed membership.
CREATE TABLE task_domain_seed_recovery_current (
 seed_id TEXT NOT NULL REFERENCES episode_continuity_seeds(seed_id),
 family TEXT NOT NULL CHECK(family IN ('task','result_settlement')),
 record_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 content_digest TEXT NOT NULL CHECK(length(content_digest)=64),
 projection_digest TEXT NOT NULL CHECK(length(projection_digest)=64),
 fixed_content_digest TEXT NOT NULL CHECK(length(fixed_content_digest)=64),
 fixed_projection_digest TEXT NOT NULL CHECK(length(fixed_projection_digest)=64),
 PRIMARY KEY(seed_id,family,record_id)
);

-- Actual Message ACK metadata. This is a source selector, never body authority.
CREATE TABLE task_message_source_inventory (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 origin TEXT NOT NULL CHECK(origin IN ('fresh_empty','fixed')),
 seed_id TEXT,
 query_inventory_id TEXT,
 CHECK((origin='fresh_empty' AND seed_id IS NULL) OR (origin='fixed' AND seed_id IS NOT NULL)),
 PRIMARY KEY(agent_id,identity_incarnation)
);
CREATE TABLE task_message_sources (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 source_id TEXT NOT NULL,
 task_id TEXT NOT NULL,
 is_result INTEGER NOT NULL CHECK(is_result IN (0,1)),
 message_status TEXT NOT NULL CHECK(message_status IN ('queued','running','cancelling','completed','failed','cancelled','interrupted')),
 source_locator_json TEXT NOT NULL,
 PRIMARY KEY(agent_id,identity_incarnation,source_id)
);
CREATE INDEX idx_task_message_sources_task ON task_message_sources(agent_id,identity_incarnation,task_id,is_result);

-- Received ordering for the global TaskOutput window; never a raw backfill.
CREATE TABLE task_message_order_headers (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 evidence_id TEXT NOT NULL,
 message_id TEXT,
 message_seq INTEGER,
 created_at TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 content_digest TEXT NOT NULL CHECK(length(content_digest)=64),
 PRIMARY KEY(agent_id,identity_incarnation,evidence_id)
);
CREATE INDEX idx_task_message_order_headers_recent ON task_message_order_headers(agent_id,identity_incarnation,message_seq DESC,created_at DESC,message_id ASC);


-- Accepted Task query relations. Fixed / JSON-import members have birth 0;
-- native first INSERT ACKs allocate the durable same-incarnation cursor.
CREATE TABLE task_query_task_inventory (
 agent_id TEXT NOT NULL, identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 origin TEXT NOT NULL CHECK(origin IN ('fresh_empty','fixed')),
 query_inventory_id TEXT, complete INTEGER NOT NULL CHECK(complete IN (0,1)),
 PRIMARY KEY(agent_id,identity_incarnation)
);
CREATE TABLE task_query_birth_cursors (
 agent_id TEXT NOT NULL, identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 high_water INTEGER NOT NULL CHECK(high_water>=0),
 PRIMARY KEY(agent_id,identity_incarnation)
);
CREATE TABLE task_query_task_births (
 agent_id TEXT NOT NULL, identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 task_id TEXT NOT NULL, birth_seq INTEGER NOT NULL CHECK(birth_seq>=0),
 PRIMARY KEY(agent_id,identity_incarnation,task_id)
);
CREATE INDEX idx_task_query_birth_seq ON task_query_task_births(agent_id,identity_incarnation,birth_seq,task_id);
CREATE TABLE task_query_task_members (
 agent_id TEXT NOT NULL, identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 task_id TEXT NOT NULL, content_digest TEXT NOT NULL CHECK(length(content_digest)=64),
 projection_digest TEXT NOT NULL CHECK(length(projection_digest)=64),
 reply_message_id TEXT, created_at TEXT NOT NULL,
 parent_turn_id TEXT, task_status TEXT NOT NULL DEFAULT 'queued',
 header_digest TEXT CHECK(header_digest IS NULL OR length(header_digest)=64),
 PRIMARY KEY(agent_id,identity_incarnation,task_id)
);
CREATE INDEX idx_task_query_replies ON task_query_task_members(agent_id,identity_incarnation,reply_message_id,created_at,task_id);
CREATE INDEX idx_task_query_parent_turn ON task_query_task_members(agent_id,identity_incarnation,parent_turn_id,task_id);

-- Query metadata only: actual original Control completion, no Task/body authority.
CREATE TABLE task_status_control_sources (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 command_id TEXT NOT NULL,
 obligation_id TEXT NOT NULL,
 task_id TEXT NOT NULL,
 target_generation_json TEXT NOT NULL CHECK(json_valid(target_generation_json)),
 fields_json TEXT NOT NULL CHECK(json_valid(fields_json) AND json_type(fields_json)='object'),
 updated_at TEXT NOT NULL,
 PRIMARY KEY(agent_id,identity_incarnation,command_id,obligation_id)
);
CREATE INDEX idx_task_status_control_source_task
 ON task_status_control_sources(agent_id,identity_incarnation,task_id,updated_at,command_id);

-- Read-only projection of actual delegation ACKs; global child ordering is preserved.
CREATE TABLE task_status_delegation_sources (
 delegation_id TEXT PRIMARY KEY,
 child_agent_id TEXT NOT NULL,
 parent_work_item_id TEXT NOT NULL,
 child_work_item_id TEXT NOT NULL,
 created_at TEXT NOT NULL,
 updated_at TEXT NOT NULL
);
CREATE INDEX idx_task_status_delegation_child
 ON task_status_delegation_sources(child_agent_id,updated_at DESC,created_at DESC,delegation_id ASC);

-- Original TaskStatus query metadata cut. It grants no Task or Input membership.
CREATE TABLE task_status_source_inventory (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 control_observed INTEGER NOT NULL CHECK(control_observed IN (0,1)),
 delegation_observed INTEGER NOT NULL CHECK(delegation_observed IN (0,1)),
 control_count INTEGER NOT NULL DEFAULT 0 CHECK(control_count>=0),
 delegation_count INTEGER NOT NULL DEFAULT 0 CHECK(delegation_count>=0),
 metadata_digest TEXT NOT NULL DEFAULT '',
 sealed INTEGER NOT NULL CHECK(sealed IN (0,1)),
 CHECK(sealed=0 OR length(metadata_digest)=64)
);
CREATE TABLE task_status_fixed_controls (
 agent_id TEXT NOT NULL, identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 command_id TEXT NOT NULL, obligation_id TEXT NOT NULL, task_id TEXT NOT NULL,
 target_generation_json TEXT NOT NULL CHECK(json_valid(target_generation_json)),
 fields_json TEXT NOT NULL CHECK(json_valid(fields_json) AND json_type(fields_json)='object'),
 updated_at TEXT NOT NULL,
 PRIMARY KEY(agent_id,identity_incarnation,command_id,obligation_id)
);
CREATE TABLE task_status_fixed_delegations (
 delegation_id TEXT PRIMARY KEY, child_agent_id TEXT NOT NULL,
 parent_work_item_id TEXT NOT NULL, child_work_item_id TEXT NOT NULL,
 created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TRIGGER task_status_source_inventory_immutable_insert BEFORE INSERT ON task_status_source_inventory WHEN EXISTS(SELECT 1 FROM task_status_source_inventory WHERE singleton=1 AND sealed=1) BEGIN SELECT RAISE(ABORT,'fixed TaskStatus source inventory is immutable'); END;
CREATE TRIGGER task_status_source_inventory_immutable_update BEFORE UPDATE ON task_status_source_inventory WHEN EXISTS(SELECT 1 FROM task_status_source_inventory WHERE singleton=1 AND sealed=1) BEGIN SELECT RAISE(ABORT,'fixed TaskStatus source inventory is immutable'); END;
CREATE TRIGGER task_status_source_inventory_immutable_delete BEFORE DELETE ON task_status_source_inventory WHEN EXISTS(SELECT 1 FROM task_status_source_inventory WHERE singleton=1 AND sealed=1) BEGIN SELECT RAISE(ABORT,'fixed TaskStatus source inventory is immutable'); END;
CREATE TRIGGER task_status_fixed_controls_immutable_insert BEFORE INSERT ON task_status_fixed_controls WHEN EXISTS(SELECT 1 FROM task_status_source_inventory WHERE singleton=1 AND sealed=1) BEGIN SELECT RAISE(ABORT,'fixed TaskStatus source inventory is immutable'); END;
CREATE TRIGGER task_status_fixed_controls_immutable_update BEFORE UPDATE ON task_status_fixed_controls WHEN EXISTS(SELECT 1 FROM task_status_source_inventory WHERE singleton=1 AND sealed=1) BEGIN SELECT RAISE(ABORT,'fixed TaskStatus source inventory is immutable'); END;
CREATE TRIGGER task_status_fixed_controls_immutable_delete BEFORE DELETE ON task_status_fixed_controls WHEN EXISTS(SELECT 1 FROM task_status_source_inventory WHERE singleton=1 AND sealed=1) BEGIN SELECT RAISE(ABORT,'fixed TaskStatus source inventory is immutable'); END;
CREATE TRIGGER task_status_fixed_delegations_immutable_insert BEFORE INSERT ON task_status_fixed_delegations WHEN EXISTS(SELECT 1 FROM task_status_source_inventory WHERE singleton=1 AND sealed=1) BEGIN SELECT RAISE(ABORT,'fixed TaskStatus source inventory is immutable'); END;
CREATE TRIGGER task_status_fixed_delegations_immutable_update BEFORE UPDATE ON task_status_fixed_delegations WHEN EXISTS(SELECT 1 FROM task_status_source_inventory WHERE singleton=1 AND sealed=1) BEGIN SELECT RAISE(ABORT,'fixed TaskStatus source inventory is immutable'); END;
CREATE TRIGGER task_status_fixed_delegations_immutable_delete BEFORE DELETE ON task_status_fixed_delegations WHEN EXISTS(SELECT 1 FROM task_status_source_inventory WHERE singleton=1 AND sealed=1) BEGIN SELECT RAISE(ABORT,'fixed TaskStatus source inventory is immutable'); END;

-- Dependency discovery for global delegation updates, never mutation authority.
CREATE TABLE task_status_child_observers (
 agent_id TEXT NOT NULL, identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 task_id TEXT NOT NULL, child_agent_id TEXT NOT NULL,
 task_revision INTEGER NOT NULL CHECK(task_revision>0),
 content_digest TEXT NOT NULL CHECK(length(content_digest)=64),
 projection_digest TEXT NOT NULL CHECK(length(projection_digest)=64),
 PRIMARY KEY(agent_id,identity_incarnation,task_id)
);
CREATE INDEX idx_task_status_child_observer_scope
 ON task_status_child_observers(child_agent_id,agent_id,identity_incarnation,task_id);
