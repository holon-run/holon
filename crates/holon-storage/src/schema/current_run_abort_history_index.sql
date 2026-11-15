CREATE INDEX IF NOT EXISTS idx_turn_records_operator_abort_candidates
    ON turn_records(agent_id)
    WHERE CASE WHEN json_valid(payload_json)
          THEN json_extract(payload_json,'$.terminal.reason')='operator_aborted'
            AND json_extract(payload_json,'$.terminal.kind') IN ('interrupted','aborted')
          ELSE 1 END;
