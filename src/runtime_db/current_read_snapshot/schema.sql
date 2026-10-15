-- Qualification metadata is fixed at cutover/import, then advanced only by actual writes.
CREATE TABLE current_read_fixed_captures (
 seed_id TEXT NOT NULL REFERENCES episode_continuity_seeds(seed_id),
 capture_phase TEXT NOT NULL CHECK(capture_phase IN ('upgrade','first_import','legacy_import')),
 queue_available INTEGER NOT NULL CHECK(queue_available IN(0,1)),
 event_available INTEGER NOT NULL CHECK(event_available IN(0,1)),
 member_count INTEGER NOT NULL CHECK(member_count>=0), sealed INTEGER NOT NULL CHECK(sealed IN (0,1)),
 PRIMARY KEY(seed_id,capture_phase)
);
CREATE TABLE current_read_fixed_members (
 seed_id TEXT NOT NULL, capture_phase TEXT NOT NULL, family TEXT NOT NULL CHECK(family IN ('queue','event')),
 record_id TEXT NOT NULL, agent_id TEXT NOT NULL,
 content_digest TEXT NOT NULL CHECK(length(content_digest)=64),
 projection_digest TEXT NOT NULL CHECK(length(projection_digest)=64),
 status TEXT NOT NULL, sort_seq INTEGER NOT NULL, sort_at TEXT NOT NULL, created_at TEXT NOT NULL,
 PRIMARY KEY(seed_id,capture_phase,family,record_id),
 FOREIGN KEY(seed_id,capture_phase) REFERENCES current_read_fixed_captures(seed_id,capture_phase)
);
CREATE INDEX idx_current_read_fixed_owner ON current_read_fixed_members(agent_id,family,record_id);
CREATE TABLE current_read_import_inventory (
 agent_id TEXT NOT NULL, family TEXT NOT NULL CHECK(family IN ('queue','event')),record_id TEXT NOT NULL,
 content_digest TEXT NOT NULL CHECK(length(content_digest)=64),projection_digest TEXT NOT NULL CHECK(length(projection_digest)=64),
 PRIMARY KEY(agent_id,family,record_id)
);
CREATE TABLE current_read_members (
 agent_id TEXT NOT NULL,identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 family TEXT NOT NULL CHECK(family IN ('queue','event')),record_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0), availability TEXT NOT NULL CHECK(availability IN ('present','pruned')),
 content_digest TEXT CHECK(content_digest IS NULL OR length(content_digest)=64),
 projection_digest TEXT CHECK(projection_digest IS NULL OR length(projection_digest)=64),
 status TEXT NOT NULL,sort_seq INTEGER NOT NULL,sort_at TEXT NOT NULL,created_at TEXT NOT NULL,
 CHECK((availability='present' AND content_digest IS NOT NULL AND projection_digest IS NOT NULL)
 OR (availability='pruned' AND content_digest IS NULL AND projection_digest IS NULL)),
 PRIMARY KEY(agent_id,identity_incarnation,family,record_id)
);
CREATE INDEX idx_current_read_members_heads ON current_read_members(agent_id,identity_incarnation,family,sort_seq DESC,sort_at DESC);
CREATE TABLE current_read_current (
 agent_id TEXT NOT NULL,identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 revision INTEGER NOT NULL CHECK(revision>0),coverage_json TEXT NOT NULL,
 freshness TEXT NOT NULL DEFAULT 'current' CHECK(freshness IN('current','binding_invalidated','records_invalidated')),
 PRIMARY KEY(agent_id,identity_incarnation)
);
-- A real State-only ACK cannot attest old Queue/Event+State coherence. This
-- bounded marker grants no members; closed finalizers restore it in the same TX.
CREATE TRIGGER current_read_state_insert AFTER INSERT ON agent_states
 BEGIN UPDATE current_read_current SET freshness=CASE WHEN freshness='records_invalidated' THEN freshness ELSE 'binding_invalidated' END WHERE agent_id=NEW.agent_id; END;
CREATE TRIGGER current_read_state_update AFTER UPDATE ON agent_states
 WHEN OLD.payload_json IS NOT NEW.payload_json OR OLD.agent_id IS NOT NEW.agent_id
 BEGIN UPDATE current_read_current SET freshness=CASE WHEN freshness='records_invalidated' THEN freshness ELSE 'binding_invalidated' END WHERE agent_id IN(OLD.agent_id,NEW.agent_id); END;
CREATE TRIGGER current_read_state_delete AFTER DELETE ON agent_states
 BEGIN UPDATE current_read_current SET freshness=CASE WHEN freshness='records_invalidated' THEN freshness ELSE 'binding_invalidated' END WHERE agent_id=OLD.agent_id; END;
CREATE TRIGGER current_read_identity_insert AFTER INSERT ON agent_identities
 BEGIN UPDATE current_read_current SET freshness=CASE WHEN freshness='records_invalidated' THEN freshness ELSE 'binding_invalidated' END WHERE agent_id=NEW.agent_id; END;
CREATE TRIGGER current_read_identity_update AFTER UPDATE ON agent_identities
 WHEN OLD.payload_json IS NOT NEW.payload_json OR OLD.agent_id IS NOT NEW.agent_id OR OLD.status IS NOT NEW.status
 BEGIN UPDATE current_read_current SET freshness=CASE WHEN freshness='records_invalidated' THEN freshness ELSE 'binding_invalidated' END WHERE agent_id IN(OLD.agent_id,NEW.agent_id); END;
CREATE TRIGGER current_read_identity_delete AFTER DELETE ON agent_identities
 BEGIN UPDATE current_read_current SET freshness=CASE WHEN freshness='records_invalidated' THEN freshness ELSE 'binding_invalidated' END WHERE agent_id=OLD.agent_id; END;
CREATE TRIGGER current_read_fixed_capture_insert BEFORE INSERT ON current_read_fixed_captures WHEN
 EXISTS(SELECT 1 FROM current_read_fixed_captures WHERE seed_id=NEW.seed_id AND capture_phase=NEW.capture_phase AND sealed=1)
 BEGIN SELECT RAISE(ABORT,'CurrentRead fixed capture immutable'); END;
CREATE TRIGGER current_read_fixed_capture_update BEFORE UPDATE ON current_read_fixed_captures WHEN OLD.sealed=1 OR
 EXISTS(SELECT 1 FROM current_read_fixed_captures WHERE seed_id=NEW.seed_id AND capture_phase=NEW.capture_phase AND sealed=1)
 BEGIN SELECT RAISE(ABORT,'CurrentRead fixed capture immutable'); END;
CREATE TRIGGER current_read_fixed_capture_delete BEFORE DELETE ON current_read_fixed_captures WHEN OLD.sealed=1
 BEGIN SELECT RAISE(ABORT,'CurrentRead fixed capture immutable'); END;
CREATE TRIGGER current_read_fixed_member_insert BEFORE INSERT ON current_read_fixed_members WHEN
 (SELECT sealed FROM current_read_fixed_captures WHERE seed_id=NEW.seed_id AND capture_phase=NEW.capture_phase)=1
 BEGIN SELECT RAISE(ABORT,'CurrentRead fixed member immutable'); END;
CREATE TRIGGER current_read_fixed_member_update BEFORE UPDATE ON current_read_fixed_members
 BEGIN SELECT RAISE(ABORT,'CurrentRead fixed member immutable'); END;
CREATE TRIGGER current_read_fixed_member_delete BEFORE DELETE ON current_read_fixed_members
 BEGIN SELECT RAISE(ABORT,'CurrentRead fixed member immutable'); END;
