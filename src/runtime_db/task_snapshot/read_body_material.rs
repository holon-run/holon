//! Raw normalized columns and complete native nodes precede Task body material.
use super::*;
#[path = "written_row_material.rs"]
mod written_row_material;

impl TaskReadMaterial<'_> {
    pub(in crate::runtime_db::task_snapshot) fn admit_stored_tx(
        &mut self,
        tx: &Transaction<'_>,
        agent: &str,
        key: &TaskKey,
    ) -> Result<Option<usize>> {
        if self.budget.is_none() {
            return checkpoint_material_size_on(tx, key);
        }
        let (owner_sql, _, native_sql, _) = stored_material_sql(key.family);
        self.charge_record(key, 0)?;
        let owner: Option<bool> = tx
            .query_row(owner_sql, params![key.id, agent], |row| row.get(0))
            .optional()?;
        let Some(owner) = owner else {
            return Ok(None);
        };
        ensure!(owner, TaskSnapshotError::ForeignRecord(key.clone()));
        let exact = self
            .admit_projection_inner_tx(tx, key)?
            .context("Task body disappeared")?;
        let payload_nodes: usize = tx.query_row(native_sql, [&key.id], |row| row.get(0))?;
        self.charge(0, payload_nodes, 8, false)?;
        Ok(Some(exact))
    }
    /// Scalar sizing never reads another owner's JSON body or grants membership.
    pub(in crate::runtime_db) fn admit_projection_size_tx(
        &mut self,
        tx: &Transaction<'_>,
        key: &TaskKey,
    ) -> Result<Option<usize>> {
        if self.budget.is_none() {
            return checkpoint_material_size_on(tx, key);
        }
        self.charge_record(key, 0)?;
        self.admit_projection_inner_tx(tx, key)
    }
    fn admit_projection_inner_tx(
        &mut self,
        tx: &Transaction<'_>,
        key: &TaskKey,
    ) -> Result<Option<usize>> {
        let (_, raw_sql, _, fields) = stored_material_sql(key.family);
        let raw: Option<(usize, usize)> = tx
            .query_row(raw_sql, [&key.id], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        let Some((payload, normalized)) = raw else {
            return Ok(None);
        };
        let scalar = payload
            .checked_add(normalized)
            .and_then(|bytes| bytes.checked_add(fields.checked_mul(64)?))
            .context("Task scalar material overflow")?;
        // Eight reserved copies cover SQLite's first <=6x escaped projection.
        self.charge(scalar, 0, 8, false)?;
        self.charge(0, fields + 1, 8, false)?;
        let exact = checkpoint_material_size_on(tx, key)?.context("Task body disappeared")?;
        if exact > scalar {
            self.charge(exact - scalar, 0, 8, false)?;
        }
        Ok(Some(exact))
    }
}

fn stored_material_sql(family: TaskFamily) -> (&'static str, &'static str, &'static str, usize) {
    match family {
            TaskFamily::Task => (
                "SELECT owner_agent_id=?2 FROM tasks WHERE task_id=?1",
                "SELECT length(CAST(payload_json AS BLOB)),COALESCE(length(CAST(task_id AS BLOB)),0)+COALESCE(length(CAST(owner_agent_id AS BLOB)),0)+COALESCE(length(CAST(parent_agent_id AS BLOB)),0)+COALESCE(length(CAST(child_agent_id AS BLOB)),0)+COALESCE(length(CAST(kind AS BLOB)),0)+COALESCE(length(CAST(status AS BLOB)),0)+COALESCE(length(CAST(summary AS BLOB)),0)+COALESCE(length(CAST(input_target AS BLOB)),0)+COALESCE(length(CAST(wait_policy AS BLOB)),0)+COALESCE(length(CAST(output_path AS BLOB)),0)+COALESCE(length(CAST(result_summary AS BLOB)),0)+COALESCE(length(CAST(exit_status AS BLOB)),0)+COALESCE(length(CAST(terminal_reentry AS BLOB)),0)+COALESCE(length(CAST(revision AS BLOB)),0)+COALESCE(length(CAST(created_at AS BLOB)),0)+COALESCE(length(CAST(updated_at AS BLOB)),0)+COALESCE(length(CAST(completed_at AS BLOB)),0)+COALESCE(length(CAST(last_message_id AS BLOB)),0)+COALESCE(length(CAST(last_turn_id AS BLOB)),0)+COALESCE(length(CAST(causation_id AS BLOB)),0)+COALESCE(length(CAST(correlation_id AS BLOB)),0) FROM tasks WHERE task_id=?1",
                "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM tasks t,json_tree(t.payload_json) j WHERE t.task_id=?1",
                21usize,
            ),
            TaskFamily::ResultSettlement => (
                "SELECT agent_id=?2 FROM task_result_settlements WHERE result_identity=?1",
                "SELECT length(CAST(payload_json AS BLOB)),COALESCE(length(CAST(result_identity AS BLOB)),0)+COALESCE(length(CAST(agent_id AS BLOB)),0)+COALESCE(length(CAST(task_id AS BLOB)),0)+COALESCE(length(CAST(message_id AS BLOB)),0)+COALESCE(length(CAST(work_item_id AS BLOB)),0)+COALESCE(length(CAST(rejoin_generation AS BLOB)),0)+COALESCE(length(CAST(parent_turn_id AS BLOB)),0)+COALESCE(length(CAST(state AS BLOB)),0)+COALESCE(length(CAST(activation_id AS BLOB)),0)+COALESCE(length(CAST(disposition AS BLOB)),0)+COALESCE(length(CAST(created_at AS BLOB)),0)+COALESCE(length(CAST(updated_at AS BLOB)),0)+COALESCE(length(CAST(admitted_at AS BLOB)),0)+COALESCE(length(CAST(settled_at AS BLOB)),0)+COALESCE(length(CAST(deferred_reason AS BLOB)),0)+COALESCE(length(CAST(deferred_at AS BLOB)),0)+COALESCE(length(CAST(next_recheck_at AS BLOB)),0) FROM task_result_settlements WHERE result_identity=?1",
                "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM task_result_settlements t,json_tree(t.payload_json) j WHERE t.result_identity=?1",
                17usize,
            ),
    }
}
