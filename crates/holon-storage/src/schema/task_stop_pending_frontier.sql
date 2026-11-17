-- Keep the exact pending predicate; completed Stop history is not a frontier.
CREATE INDEX IF NOT EXISTS idx_task_stop_pending_frontier
 ON task_stop_effects(agent_id,identity_incarnation,effect_seq)
 WHERE physical_state='pending' OR child_state='pending' OR worktree_state='pending' OR result_state='pending';
