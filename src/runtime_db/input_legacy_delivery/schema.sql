CREATE TABLE IF NOT EXISTS input_legacy_delivery_captures (
  capture_id TEXT PRIMARY KEY,
  table_available INTEGER NOT NULL CHECK(table_available IN (0,1)),
  member_count INTEGER NOT NULL CHECK(member_count>=0),
  metadata_digest TEXT NOT NULL,
  sealed INTEGER NOT NULL CHECK(sealed IN (0,1))
);
CREATE TABLE IF NOT EXISTS input_legacy_delivery_fixed_members (
  capture_id TEXT NOT NULL REFERENCES input_legacy_delivery_captures(capture_id),
  record_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  identity_incarnation INTEGER CHECK(identity_incarnation IS NULL OR identity_incarnation>0),
  seed_id TEXT REFERENCES episode_continuity_seeds(seed_id),
  work_item_id TEXT NOT NULL,
  created_at TEXT NOT NULL,
  text_nonempty INTEGER NOT NULL CHECK(text_nonempty IN (0,1)),
  fingerprint_json TEXT NOT NULL,
  content_digest TEXT NOT NULL,
  PRIMARY KEY(capture_id,record_id)
);
CREATE INDEX IF NOT EXISTS input_legacy_delivery_fixed_scope
  ON input_legacy_delivery_fixed_members(agent_id,identity_incarnation,work_item_id,created_at DESC,record_id DESC);
CREATE TRIGGER IF NOT EXISTS input_legacy_delivery_capture_insert_sealed
 BEFORE INSERT ON input_legacy_delivery_captures
 WHEN EXISTS(SELECT 1 FROM input_legacy_delivery_captures WHERE capture_id=NEW.capture_id AND sealed=1)
 BEGIN SELECT RAISE(ABORT,'LegacyDelivery capture is immutable'); END;
CREATE TRIGGER IF NOT EXISTS input_legacy_delivery_capture_update_sealed
 BEFORE UPDATE ON input_legacy_delivery_captures
 WHEN OLD.sealed=1 OR EXISTS(SELECT 1 FROM input_legacy_delivery_captures WHERE capture_id=NEW.capture_id AND sealed=1)
 BEGIN SELECT RAISE(ABORT,'LegacyDelivery capture is immutable'); END;
CREATE TRIGGER IF NOT EXISTS input_legacy_delivery_capture_delete_sealed
 BEFORE DELETE ON input_legacy_delivery_captures WHEN OLD.sealed=1
 BEGIN SELECT RAISE(ABORT,'LegacyDelivery capture is immutable'); END;
CREATE TRIGGER IF NOT EXISTS input_legacy_delivery_member_insert_sealed
 BEFORE INSERT ON input_legacy_delivery_fixed_members
 WHEN EXISTS(SELECT 1 FROM input_legacy_delivery_captures WHERE capture_id=NEW.capture_id AND sealed=1)
 BEGIN SELECT RAISE(ABORT,'LegacyDelivery member is immutable'); END;
CREATE TRIGGER IF NOT EXISTS input_legacy_delivery_member_update_sealed
 BEFORE UPDATE ON input_legacy_delivery_fixed_members
 WHEN EXISTS(SELECT 1 FROM input_legacy_delivery_captures WHERE capture_id=OLD.capture_id AND sealed=1)
   OR EXISTS(SELECT 1 FROM input_legacy_delivery_captures WHERE capture_id=NEW.capture_id AND sealed=1)
 BEGIN SELECT RAISE(ABORT,'LegacyDelivery member is immutable'); END;
CREATE TRIGGER IF NOT EXISTS input_legacy_delivery_member_delete_sealed
 BEFORE DELETE ON input_legacy_delivery_fixed_members
 WHEN EXISTS(SELECT 1 FROM input_legacy_delivery_captures WHERE capture_id=OLD.capture_id AND sealed=1)
 BEGIN SELECT RAISE(ABORT,'LegacyDelivery member is immutable'); END;

-- Private initial pre-Host SQL cohort. The complete/failed business checkpoint
-- remains independent; retries never recapture new raw tails or a new owner.
CREATE TABLE IF NOT EXISTS legacy_episode_import_captures (
  capture_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  identity_incarnation INTEGER CHECK(identity_incarnation IS NULL OR identity_incarnation>0),
  seed_id TEXT REFERENCES episode_continuity_seeds(seed_id),
  member_count INTEGER NOT NULL CHECK(member_count>=0),
  metadata_digest TEXT NOT NULL,
  sealed INTEGER NOT NULL CHECK(sealed IN (0,1)),
  empty_original INTEGER NOT NULL DEFAULT 0 CHECK(empty_original IN (0,1)),
  CHECK((empty_original=0 AND (identity_incarnation IS NOT NULL OR seed_id IS NOT NULL))
     OR (empty_original=1 AND identity_incarnation IS NULL AND seed_id IS NULL AND member_count=0))
);
CREATE TABLE IF NOT EXISTS legacy_episode_import_members (
  capture_id TEXT NOT NULL REFERENCES legacy_episode_import_captures(capture_id),
  family TEXT NOT NULL CHECK(family IN ('message','transcript','tool','brief','delivery')),
  ordinal INTEGER NOT NULL CHECK(ordinal>=0),
  record_id TEXT NOT NULL,
  raw_digest TEXT NOT NULL CHECK(length(CAST(raw_digest AS BLOB))=64 AND raw_digest NOT GLOB '*[^0-9a-f]*'),
  PRIMARY KEY(capture_id,family,ordinal),
  UNIQUE(capture_id,family,record_id)
);
CREATE TRIGGER IF NOT EXISTS legacy_episode_import_capture_insert_sealed
 BEFORE INSERT ON legacy_episode_import_captures
 WHEN EXISTS(SELECT 1 FROM legacy_episode_import_captures WHERE capture_id=NEW.capture_id AND sealed=1)
 BEGIN SELECT RAISE(ABORT,'original episode capture is immutable'); END;
CREATE TRIGGER IF NOT EXISTS legacy_episode_import_capture_update_sealed
 BEFORE UPDATE ON legacy_episode_import_captures
 WHEN OLD.sealed=1 OR EXISTS(SELECT 1 FROM legacy_episode_import_captures WHERE capture_id=NEW.capture_id AND sealed=1)
 BEGIN SELECT RAISE(ABORT,'original episode capture is immutable'); END;
CREATE TRIGGER IF NOT EXISTS legacy_episode_import_capture_delete_sealed
 BEFORE DELETE ON legacy_episode_import_captures WHEN OLD.sealed=1
 BEGIN SELECT RAISE(ABORT,'original episode capture is immutable'); END;
CREATE TRIGGER IF NOT EXISTS legacy_episode_import_member_insert_sealed
 BEFORE INSERT ON legacy_episode_import_members
 WHEN EXISTS(SELECT 1 FROM legacy_episode_import_captures WHERE capture_id=NEW.capture_id AND sealed=1)
 BEGIN SELECT RAISE(ABORT,'original episode member is immutable'); END;
CREATE TRIGGER IF NOT EXISTS legacy_episode_import_member_update_sealed
 BEFORE UPDATE ON legacy_episode_import_members
 WHEN EXISTS(SELECT 1 FROM legacy_episode_import_captures WHERE capture_id=OLD.capture_id AND sealed=1)
 OR EXISTS(SELECT 1 FROM legacy_episode_import_captures WHERE capture_id=NEW.capture_id AND sealed=1)
 BEGIN SELECT RAISE(ABORT,'original episode member is immutable'); END;
CREATE TRIGGER IF NOT EXISTS legacy_episode_import_member_delete_sealed
 BEFORE DELETE ON legacy_episode_import_members
 WHEN EXISTS(SELECT 1 FROM legacy_episode_import_captures WHERE capture_id=OLD.capture_id AND sealed=1)
 BEGIN SELECT RAISE(ABORT,'original episode member is immutable'); END;

CREATE TABLE IF NOT EXISTS legacy_episode_wait_captures (
 capture_id TEXT PRIMARY KEY,
 source_capture_id TEXT NOT NULL REFERENCES legacy_episode_import_captures(capture_id),
 member_count INTEGER NOT NULL CHECK(member_count>=0),
 metadata_digest TEXT NOT NULL,
 sealed INTEGER NOT NULL CHECK(sealed IN (0,1))
);
CREATE TABLE IF NOT EXISTS legacy_episode_wait_members (
 capture_id TEXT NOT NULL REFERENCES legacy_episode_wait_captures(capture_id),
 ordinal INTEGER NOT NULL CHECK(ordinal>=0),
 record_id TEXT NOT NULL,
 normalized_digest TEXT NOT NULL CHECK(length(CAST(normalized_digest AS BLOB))=64 AND normalized_digest NOT GLOB '*[^0-9a-f]*'),
 PRIMARY KEY(capture_id,ordinal),
 UNIQUE(capture_id,record_id)
);
CREATE TRIGGER IF NOT EXISTS legacy_episode_wait_capture_insert_sealed
 BEFORE INSERT ON legacy_episode_wait_captures
 WHEN EXISTS(SELECT 1 FROM legacy_episode_wait_captures WHERE capture_id=NEW.capture_id AND sealed=1)
 BEGIN SELECT RAISE(ABORT,'original Wait capture is immutable'); END;
CREATE TRIGGER IF NOT EXISTS legacy_episode_wait_capture_update_sealed
 BEFORE UPDATE ON legacy_episode_wait_captures
 WHEN OLD.sealed=1 OR EXISTS(SELECT 1 FROM legacy_episode_wait_captures WHERE capture_id=NEW.capture_id AND sealed=1)
 BEGIN SELECT RAISE(ABORT,'original Wait capture is immutable'); END;
CREATE TRIGGER IF NOT EXISTS legacy_episode_wait_capture_delete_sealed
 BEFORE DELETE ON legacy_episode_wait_captures WHEN OLD.sealed=1
 BEGIN SELECT RAISE(ABORT,'original Wait capture is immutable'); END;
CREATE TRIGGER IF NOT EXISTS legacy_episode_wait_member_insert_sealed
 BEFORE INSERT ON legacy_episode_wait_members
 WHEN EXISTS(SELECT 1 FROM legacy_episode_wait_captures WHERE capture_id=NEW.capture_id AND sealed=1)
 BEGIN SELECT RAISE(ABORT,'original Wait member is immutable'); END;
CREATE TRIGGER IF NOT EXISTS legacy_episode_wait_member_update_sealed
 BEFORE UPDATE ON legacy_episode_wait_members
 WHEN EXISTS(SELECT 1 FROM legacy_episode_wait_captures WHERE capture_id=OLD.capture_id AND sealed=1)
 OR EXISTS(SELECT 1 FROM legacy_episode_wait_captures WHERE capture_id=NEW.capture_id AND sealed=1)
 BEGIN SELECT RAISE(ABORT,'original Wait member is immutable'); END;
CREATE TRIGGER IF NOT EXISTS legacy_episode_wait_member_delete_sealed
 BEFORE DELETE ON legacy_episode_wait_members
 WHEN EXISTS(SELECT 1 FROM legacy_episode_wait_captures WHERE capture_id=OLD.capture_id AND sealed=1)
 BEGIN SELECT RAISE(ABORT,'original Wait member is immutable'); END;

-- Complete accepted metadata only. No body, native birth or new read authority.
CREATE TABLE IF NOT EXISTS input_legacy_delivery_scopes (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 seed_id TEXT,
 namespace_available INTEGER NOT NULL CHECK(namespace_available IN (0,1)),
 PRIMARY KEY(agent_id,identity_incarnation)
);
CREATE TABLE IF NOT EXISTS input_legacy_delivery_current_sources (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 record_id TEXT NOT NULL,
 work_item_id TEXT NOT NULL,
 created_at TEXT NOT NULL,
 text_nonempty INTEGER NOT NULL CHECK(text_nonempty IN (0,1)),
 fingerprint_json TEXT NOT NULL,
 content_digest TEXT NOT NULL CHECK(length(CAST(content_digest AS BLOB))=64 AND content_digest NOT GLOB '*[^0-9a-f]*'),
 source_revision INTEGER NOT NULL CHECK(source_revision>=0),
 availability TEXT NOT NULL CHECK(availability IN ('present','pruned')),
 PRIMARY KEY(agent_id,identity_incarnation,record_id)
);
CREATE INDEX IF NOT EXISTS input_legacy_delivery_current_work_head
 ON input_legacy_delivery_current_sources(agent_id,identity_incarnation,work_item_id,created_at DESC,record_id DESC);
