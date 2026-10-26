//! Accepted cleanup headers are written at the actual original completion.
use super::*;
use crate::runtime_db::control_transition::OriginalControlWorktreeCompletion;

pub(in crate::runtime_db) fn register_control_worktree_completion_tx(
    tx: &Transaction<'_>,
    observed: &OriginalControlWorktreeCompletion<'_, '_>,
) -> Result<()> {
    let (key, obligation_id) = observed.source_in(tx)?;
    // Read only the just-written original row. JSON projection stays in SQLite;
    // no Task, State, successor identity or complete evidence DTO is hydrated.
    let changed = tx.execute(
        "INSERT INTO task_status_control_sources(agent_id,identity_incarnation,command_id,obligation_id,task_id,target_generation_json,fields_json,updated_at)
         SELECT c.agent_id,c.identity_incarnation,c.command_id,c.obligation_id,c.target_identity,c.target_generation_json,
          COALESCE((SELECT json_group_object(e.key,CASE e.type
            WHEN 'object' THEN json(e.value) WHEN 'array' THEN json(e.value)
            WHEN 'true' THEN json('true') WHEN 'false' THEN json('false')
            WHEN 'null' THEN json('null') ELSE e.value END)
           FROM json_each(c.completion_evidence_json,'$.detail.worktree') e
           WHERE e.key IN ('cleanup_status','cleanup_reason','cleanup_error','auto_cleaned_up','changed_files','retained_for_review','actual_branch','branch_cleanup_status','branch_cleanup_error')),'{}'),c.updated_at
         FROM control_cancellation_obligations c
         WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.command_id=?3
          AND c.obligation_id=?4 AND c.target_kind='worktree' AND c.state='completed'",
        params![key.agent_id,i64::try_from(key.identity_incarnation)?,key.command_id,obligation_id],
    )?;
    ensure!(
        changed == 1,
        "Control completion omitted its actual cleanup metadata"
    );
    Ok(())
}
