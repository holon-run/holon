-- Derived lookup only: qualification still belongs to the original Turn reader.
-- Preserve every edge, including ambiguous and terminal attachments.
CREATE TABLE IF NOT EXISTS turn_activation_attachments (
  agent_id TEXT NOT NULL,
  activation_id TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  has_live INTEGER NOT NULL CHECK (has_live IN (0, 1)),
  has_idle INTEGER NOT NULL CHECK (has_idle IN (0, 1)),
  CHECK (has_live = 1 OR has_idle = 1),
  PRIMARY KEY (agent_id, activation_id, turn_id),
  FOREIGN KEY (turn_id) REFERENCES turn_records(turn_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_turn_activation_attachments_turn
  ON turn_activation_attachments(turn_id);

-- The upgrade is the only whole-history pass. Ordinary reads never backfill.
-- Malformed canonical material contributes no edge; readers still reject it.
INSERT INTO turn_activation_attachments
  (agent_id, activation_id, turn_id, has_live, has_idle)
SELECT agent_id, activation_id, turn_id, MAX(has_live), MAX(has_idle)
FROM (
  SELECT t.agent_id, t.turn_id,
    json_extract(CASE WHEN batch.type = 'object' THEN batch.value ELSE '{}' END,
      '$.activation_id') AS activation_id,
    1 AS has_live, 0 AS has_idle
  FROM turn_records t,
    json_each(CASE WHEN json_valid(t.payload_json) THEN t.payload_json ELSE '{}' END,
      '$.live_input_batches') batch
  UNION ALL
  SELECT agent_id, turn_id,
    json_extract(CASE WHEN json_valid(payload_json) THEN payload_json ELSE '{}' END,
      '$.idle_input_batch.activation_id'),
    0, 1
  FROM turn_records
)
WHERE typeof(activation_id) = 'text' AND length(activation_id) > 0
GROUP BY agent_id, activation_id, turn_id
ON CONFLICT(agent_id, activation_id, turn_id) DO UPDATE SET
  has_live = excluded.has_live, has_idle = excluded.has_idle;

CREATE TRIGGER IF NOT EXISTS turn_activation_attachments_insert
AFTER INSERT ON turn_records
BEGIN
  INSERT INTO turn_activation_attachments
    (agent_id, activation_id, turn_id, has_live, has_idle)
  SELECT NEW.agent_id, activation_id, NEW.turn_id, MAX(has_live), MAX(has_idle)
  FROM (
    SELECT json_extract(CASE WHEN batch.type = 'object' THEN batch.value ELSE '{}' END,
      '$.activation_id') AS activation_id,
      1 AS has_live, 0 AS has_idle
    FROM json_each(CASE WHEN json_valid(NEW.payload_json) THEN NEW.payload_json ELSE '{}' END,
      '$.live_input_batches') batch
    UNION ALL
    SELECT json_extract(CASE WHEN json_valid(NEW.payload_json) THEN NEW.payload_json ELSE '{}' END,
      '$.idle_input_batch.activation_id'), 0, 1
  )
  WHERE typeof(activation_id) = 'text' AND length(activation_id) > 0
  GROUP BY activation_id;
END;

CREATE TRIGGER IF NOT EXISTS turn_activation_attachments_update
AFTER UPDATE OF agent_id, turn_id, payload_json ON turn_records
BEGIN
  DELETE FROM turn_activation_attachments WHERE turn_id = OLD.turn_id;
  INSERT INTO turn_activation_attachments
    (agent_id, activation_id, turn_id, has_live, has_idle)
  SELECT NEW.agent_id, activation_id, NEW.turn_id, MAX(has_live), MAX(has_idle)
  FROM (
    SELECT json_extract(CASE WHEN batch.type = 'object' THEN batch.value ELSE '{}' END,
      '$.activation_id') AS activation_id,
      1 AS has_live, 0 AS has_idle
    FROM json_each(CASE WHEN json_valid(NEW.payload_json) THEN NEW.payload_json ELSE '{}' END,
      '$.live_input_batches') batch
    UNION ALL
    SELECT json_extract(CASE WHEN json_valid(NEW.payload_json) THEN NEW.payload_json ELSE '{}' END,
      '$.idle_input_batch.activation_id'), 0, 1
  )
  WHERE typeof(activation_id) = 'text' AND length(activation_id) > 0
  GROUP BY activation_id;
END;

CREATE TRIGGER IF NOT EXISTS turn_activation_attachments_delete
AFTER DELETE ON turn_records
BEGIN
  DELETE FROM turn_activation_attachments WHERE turn_id = OLD.turn_id;
END;
