-- Only original append ACKs write these metadata rows; old/fixed/imported
-- Event bodies are never discovered or promoted by this migration.
CREATE TABLE current_event_birth_cursors (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(typeof(identity_incarnation)='integer' AND identity_incarnation>0),
 high_water INTEGER NOT NULL CHECK(typeof(high_water)='integer' AND high_water>=0),
 PRIMARY KEY(agent_id,identity_incarnation)
);
CREATE TABLE current_event_native_births (
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL,
 event_id TEXT NOT NULL,
 birth_seq INTEGER NOT NULL CHECK(typeof(birth_seq)='integer' AND birth_seq>0),
 PRIMARY KEY(agent_id,identity_incarnation,event_id),
 UNIQUE(agent_id,identity_incarnation,birth_seq),
 FOREIGN KEY(agent_id,identity_incarnation) REFERENCES current_event_birth_cursors(agent_id,identity_incarnation)
);
CREATE INDEX current_event_native_frontier ON current_event_native_births(agent_id,identity_incarnation,birth_seq,event_id);
