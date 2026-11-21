//! A focused original Stop ACK can invalidate old coverage, never make it Ready.
use super::*;
use crate::runtime_db::task_stop::{ActualTaskStopProgress, TaskStopDomainInvalidation};

pub(crate) fn invalidate_task_scope_for_stop_progress_tx(
    tx: &Transaction<'_>,
    actual: &ActualTaskStopProgress<'_, '_>,
) -> Result<Option<TaskStopDomainInvalidation>> {
    actual.validate_presence_tx(tx)?;
    if !actual.changed() {
        return Ok(None);
    }
    let scope = &actual.record().intent.task.scope;
    let current: Option<(u64,u64)> = tx.query_row(
        "SELECT revision,json_extract(coverage_json,'$.revision') FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
        params![scope.agent_id,scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?)),
    ).optional()?;
    let Some((revision, payload_revision)) = current else {
        return Ok(None);
    };
    ensure!(
        revision > 0 && revision == payload_revision,
        "TaskStop old coverage revision corrupt"
    );
    let next = revision
        .checked_add(1)
        .context("TaskStop coverage revision overflow")?;
    loader::persist_coverage(tx, scope, &writes::empty_coverage(next))?;
    Ok(Some(TaskStopDomainInvalidation {
        base_revision: revision,
        revision: next,
    }))
}
