CREATE TABLE command_task_creation_sources (
 command_creation_id TEXT PRIMARY KEY,
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation > 0),
 task_id TEXT NOT NULL UNIQUE,
 rejoin_generation INTEGER NOT NULL CHECK(rejoin_generation > 0),
 original_task_json TEXT NOT NULL,
 original_digest TEXT NOT NULL,
 CHECK(command_creation_id=task_id)
);

-- Keep the legacy indexed Turn column for old readers of normalized SQL.
-- Its empty value is not a Turn: the positive source lives in its own column
-- and in the typed payload. No historical source is inferred or backfilled.
ALTER TABLE task_result_settlements ADD COLUMN command_creation_id TEXT
 CHECK(command_creation_id IS NULL OR
   (command_creation_id=task_id AND parent_turn_id=''));
