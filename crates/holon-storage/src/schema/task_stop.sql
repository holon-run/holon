-- Focused TaskStop responsibility; the original Task receipt is inserted last.
CREATE TABLE task_stop_effects (
 effect_seq INTEGER PRIMARY KEY AUTOINCREMENT,
 command_nonce TEXT NOT NULL UNIQUE REFERENCES input_mutation_receipts(command_nonce) DEFERRABLE INITIALLY DEFERRED,
 agent_id TEXT NOT NULL,
 identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 task_id TEXT NOT NULL,
 task_created_at TEXT NOT NULL,
 generation_digest TEXT NOT NULL CHECK(length(generation_digest)=64),
 intent_digest TEXT NOT NULL CHECK(length(intent_digest)=64),
 prepared_json TEXT NOT NULL,
 intent_json TEXT NOT NULL,
 ack_task_json TEXT NOT NULL,
 ack_task_digest TEXT NOT NULL CHECK(length(ack_task_digest)=64),
 revision INTEGER NOT NULL CHECK(revision>0),
 physical_state TEXT NOT NULL CHECK(physical_state IN ('pending','monitor_joined','missing_proof','needs_attention')),
 child_state TEXT NOT NULL CHECK(child_state IN ('not_required','pending','retired_joined','needs_attention')),
 worktree_state TEXT NOT NULL CHECK(worktree_state IN ('not_required','pending','removed','already_removed','retained','failed')),
 result_state TEXT NOT NULL CHECK(result_state IN ('not_required','pending','completed','needs_attention')),
 result_nonce TEXT UNIQUE REFERENCES input_mutation_receipts(command_nonce) DEFERRABLE INITIALLY DEFERRED,
 progress_json TEXT NOT NULL,
 created_at TEXT NOT NULL,
 updated_at TEXT NOT NULL
);
CREATE INDEX idx_task_stop_pending ON task_stop_effects(agent_id,identity_incarnation,physical_state,child_state,worktree_state,result_state);
CREATE INDEX idx_task_stop_task_head ON task_stop_effects(agent_id,identity_incarnation,task_id,generation_digest,effect_seq DESC);
