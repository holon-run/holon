CREATE TABLE IF NOT EXISTS current_run_abort_intents (
    agent_id TEXT NOT NULL,
    identity_incarnation INTEGER NOT NULL,
    run_id TEXT NOT NULL,
    source_message_id TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    PRIMARY KEY (agent_id, identity_incarnation, run_id)
);
CREATE INDEX IF NOT EXISTS idx_current_run_abort_source
    ON current_run_abort_intents(agent_id, identity_incarnation, source_message_id);
CREATE INDEX IF NOT EXISTS idx_execution_attempt_source_run
    ON execution_protocol_attempts(
        agent_id, json_extract(payload_json,'$.source_message_id'),
        json_extract(payload_json,'$.run_id'));
