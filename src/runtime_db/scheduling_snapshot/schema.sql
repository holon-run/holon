CREATE TABLE IF NOT EXISTS scheduling_fixed_manifests (
 inventory_id TEXT PRIMARY KEY, agent_id TEXT NOT NULL,
 tables_json TEXT NOT NULL, member_count INTEGER NOT NULL CHECK(member_count>=0),
 metadata_bytes INTEGER NOT NULL CHECK(metadata_bytes>=0),
 content_digest TEXT NOT NULL CHECK(length(content_digest)=64),
 sealed INTEGER NOT NULL CHECK(sealed IN (0,1))
);
CREATE TABLE IF NOT EXISTS scheduling_fixed_members (
 inventory_id TEXT NOT NULL REFERENCES scheduling_fixed_manifests(inventory_id),
 family TEXT NOT NULL CHECK(family IN ('timer','timer_wake','external_trigger')),
 record_id TEXT NOT NULL, content_digest TEXT NOT NULL CHECK(length(content_digest)=64),
 projection_digest TEXT NOT NULL CHECK(length(projection_digest)=64),
 status TEXT NOT NULL, sort_at TEXT NOT NULL, created_at TEXT NOT NULL,
 PRIMARY KEY(inventory_id,family,record_id)
);
CREATE INDEX IF NOT EXISTS idx_scheduling_fixed_id ON scheduling_fixed_members(family,record_id);
CREATE TRIGGER IF NOT EXISTS scheduling_fixed_manifest_update BEFORE UPDATE ON scheduling_fixed_manifests WHEN OLD.sealed=1 BEGIN SELECT RAISE(ABORT,'Scheduling fixed manifest immutable'); END;
CREATE TRIGGER IF NOT EXISTS scheduling_fixed_manifest_delete BEFORE DELETE ON scheduling_fixed_manifests WHEN OLD.sealed=1 BEGIN SELECT RAISE(ABORT,'Scheduling fixed manifest immutable'); END;
CREATE TRIGGER IF NOT EXISTS scheduling_fixed_member_insert BEFORE INSERT ON scheduling_fixed_members WHEN (SELECT sealed FROM scheduling_fixed_manifests WHERE inventory_id=NEW.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'Scheduling fixed member immutable'); END;
CREATE TRIGGER IF NOT EXISTS scheduling_fixed_member_update BEFORE UPDATE ON scheduling_fixed_members WHEN (SELECT sealed FROM scheduling_fixed_manifests WHERE inventory_id=OLD.inventory_id)=1 OR (SELECT sealed FROM scheduling_fixed_manifests WHERE inventory_id=NEW.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'Scheduling fixed member immutable'); END;
CREATE TRIGGER IF NOT EXISTS scheduling_fixed_member_delete BEFORE DELETE ON scheduling_fixed_members WHEN (SELECT sealed FROM scheduling_fixed_manifests WHERE inventory_id=OLD.inventory_id)=1 BEGIN SELECT RAISE(ABORT,'Scheduling fixed member immutable'); END;
CREATE TABLE IF NOT EXISTS scheduling_domain_current (
 agent_id TEXT NOT NULL, identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 revision INTEGER NOT NULL CHECK(revision>0), coverage_json TEXT NOT NULL,
 PRIMARY KEY(agent_id,identity_incarnation)
);
CREATE TABLE IF NOT EXISTS scheduling_domain_records (
 agent_id TEXT NOT NULL,identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 family TEXT NOT NULL CHECK(family IN ('timer','timer_wake','external_trigger')),
 record_id TEXT NOT NULL,revision INTEGER NOT NULL CHECK(revision>0),
 availability TEXT NOT NULL CHECK(availability IN ('present','absent','pruned')),
 content_digest TEXT CHECK(content_digest IS NULL OR length(content_digest)=64),
 projection_digest TEXT CHECK(projection_digest IS NULL OR length(projection_digest)=64),
 status TEXT, sort_at TEXT, created_at TEXT,
 CHECK(availability!='present' OR (status IS NOT NULL AND sort_at IS NOT NULL AND created_at IS NOT NULL)),
 CHECK((availability='present' AND content_digest IS NOT NULL AND projection_digest IS NOT NULL) OR (availability!='present' AND content_digest IS NULL AND projection_digest IS NULL)),
 PRIMARY KEY(agent_id,identity_incarnation,family,record_id)
);
CREATE TABLE IF NOT EXISTS scheduling_domain_captures (
 seed_id TEXT NOT NULL REFERENCES episode_continuity_seeds(seed_id),
 capture_phase TEXT NOT NULL CHECK(capture_phase IN ('upgrade','legacy_import','first_import')),
 inventory_id TEXT NOT NULL REFERENCES scheduling_fixed_manifests(inventory_id),
 PRIMARY KEY(seed_id,capture_phase)
);
CREATE TABLE IF NOT EXISTS scheduling_import_inventory (
 agent_id TEXT NOT NULL, family TEXT NOT NULL CHECK(family IN ('timer','external_trigger')),
 record_id TEXT NOT NULL, received_digest TEXT NOT NULL CHECK(length(received_digest)=64),
 content_digest TEXT NOT NULL CHECK(length(content_digest)=64),projection_digest TEXT NOT NULL CHECK(length(projection_digest)=64),
 PRIMARY KEY(agent_id,family,record_id)
);

CREATE TRIGGER IF NOT EXISTS scheduling_capture_update BEFORE UPDATE ON scheduling_domain_captures BEGIN SELECT RAISE(ABORT,'Scheduling fixed capture immutable'); END;
CREATE TRIGGER IF NOT EXISTS scheduling_capture_delete BEFORE DELETE ON scheduling_domain_captures BEGIN SELECT RAISE(ABORT,'Scheduling fixed capture immutable'); END;

-- Version updates by the original exclusive unbound cleanup actor only.
CREATE TABLE IF NOT EXISTS scheduling_seed_recovery_current (
 seed_id TEXT NOT NULL REFERENCES episode_continuity_seeds(seed_id),
 family TEXT NOT NULL CHECK(family IN ('timer','timer_wake')), record_id TEXT NOT NULL,
 original_content_digest TEXT NOT NULL CHECK(length(original_content_digest)=64),
 original_projection_digest TEXT NOT NULL CHECK(length(original_projection_digest)=64),
 content_digest TEXT NOT NULL CHECK(length(content_digest)=64),
 projection_digest TEXT NOT NULL CHECK(length(projection_digest)=64),
 revision INTEGER NOT NULL CHECK(revision>0), PRIMARY KEY(seed_id,family,record_id)
);
