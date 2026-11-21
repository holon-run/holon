-- This representation exists only for this statement. Preserve duplicate
-- entries for the SQL/Rust count check without reparsing every JSON path.
WITH prepared_metadata AS MATERIALIZED (
    SELECT jsonb(coverage_json) AS coverage_json
    FROM task_domain_current
    WHERE agent_id=?1 AND identity_incarnation=?2
)
SELECT
    (SELECT COUNT(*) FROM json_each(coverage_json,'$.recovery_sources')) +
    COALESCE((SELECT SUM((SELECT COUNT(*) FROM json_each(source.value,'$.paused_replays')))
    FROM json_each(coverage_json,'$.recovery_sources') AS source),0) +
    8*(SELECT COUNT(*) FROM json_each(coverage_json,'$.running.sources')) +
    (SELECT COUNT(*) FROM json_each(coverage_json,'$.query.born_task_seqs')) +
    (SELECT COUNT(*) FROM json_each(coverage_json,'$.query.reply_task_ids')) +
    COALESCE((SELECT SUM(json_array_length(value)) FROM json_each(coverage_json,'$.query.reply_task_ids')),0) +
    CASE WHEN EXISTS(SELECT 1 FROM json_each(coverage_json,'$.query.parent_turn_tasks')) THEN (SELECT COUNT(*)-1 FROM json_tree(coverage_json,'$.query.parent_turn_tasks')) ELSE 0 END +
    CASE WHEN EXISTS(SELECT 1 FROM json_each(coverage_json,'$.status.heads')) THEN (SELECT COUNT(*) FROM json_tree(coverage_json,'$.status')) ELSE 0 END +
    (SELECT COUNT(*) FROM json_each(coverage_json,'$.output.heads')) +
    COALESCE(json_array_length(coverage_json,'$.output.window.message_ids'),0) +
    (SELECT COUNT(*) FROM json_each(coverage_json,'$.task_metadata')) +
    (SELECT COUNT(*) FROM json_each(coverage_json,'$.result_metadata')) +
    (SELECT COUNT(*) FROM json_each(coverage_json,'$.result_dependencies')) +
    COALESCE(json_array_length(coverage_json,'$.observations'),0) +
    COALESCE((SELECT SUM(json_array_length(value)) FROM json_each(coverage_json,'$.task_result_ids')),0) +
    COALESCE((SELECT SUM(json_array_length(value)) FROM json_each(coverage_json,'$.activation_result_ids')),0) +
    COALESCE((SELECT SUM(json_array_length(value)) FROM json_each(coverage_json,'$.reply_result_ids')),0) +
    (SELECT COUNT(*) FROM json_each(coverage_json,'$.stop.records')) +
    (SELECT COUNT(*) FROM json_each(coverage_json,'$.stop.heads')) +
    COALESCE(json_array_length(coverage_json,'$.stop.pending_effect_seqs'),0)
FROM prepared_metadata
