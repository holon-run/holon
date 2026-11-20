//! Accepted Task ID / reply relations; canonical tails are never discovered.
use super::*;
use crate::types::{TaskKind, TaskRecord, TaskStatus};

pub(super) fn initialize_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    fixed: &[super::super::episode_snapshot::task_checkpoint::TaskContinuityCheckpoint],
) -> Result<()> {
    if let Some(inventory) = fixed
        .iter()
        .rev()
        .find_map(|checkpoint| checkpoint.query_inventory.as_ref())
    {
        super::super::episode_snapshot::task_checkpoint::queries::validate(tx, inventory)?;
        ensure!(
            inventory.agent_id == scope.agent_id,
            "Task relation inventory crossed agent"
        );
        let inserted=tx.execute("INSERT INTO task_query_task_inventory(agent_id,identity_incarnation,origin,query_inventory_id,complete) VALUES(?1,?2,'fixed',?3,?4) ON CONFLICT(agent_id,identity_incarnation) DO NOTHING",params![scope.agent_id,scope.identity_incarnation,inventory.id,inventory.task_table_observed])?;
        let exact:bool=tx.query_row("SELECT origin='fixed' AND query_inventory_id=?3 AND complete=?4 FROM task_query_task_inventory WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation,inventory.id,inventory.task_table_observed],|row|row.get(0))?;
        ensure!(exact, "Task query original inventory changed");
        if inserted == 1 {
            // A retained None reply row is an actual version tombstone. Repeated
            // fixed initialization cannot resurrect an old reply relationship.
            tx.execute("INSERT INTO task_query_task_members(agent_id,identity_incarnation,task_id,content_digest,projection_digest,reply_message_id,created_at,parent_turn_id,task_status,header_digest) SELECT ?1,?2,task_id,content_digest,projection_digest,reply_message_id,created_at,parent_turn_id,task_status,header_digest FROM task_query_fixed_tasks WHERE inventory_id=?3 ON CONFLICT(agent_id,identity_incarnation,task_id) DO NOTHING",params![scope.agent_id,scope.identity_incarnation,inventory.id])?;
            tx.execute("INSERT INTO task_query_task_births(agent_id,identity_incarnation,task_id,birth_seq) SELECT ?1,?2,task_id,0 FROM task_query_fixed_tasks WHERE inventory_id=?3 ON CONFLICT(agent_id,identity_incarnation,task_id) DO NOTHING",params![scope.agent_id,scope.identity_incarnation,inventory.id])?;
        }
    } else {
        tx.execute("INSERT INTO task_query_task_inventory(agent_id,identity_incarnation,origin,query_inventory_id,complete) SELECT ?1,?2,'fresh_empty',NULL,1 WHERE EXISTS(SELECT 1 FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2 AND mode='fresh_empty' AND seed_id IS NULL) ON CONFLICT(agent_id,identity_incarnation) DO NOTHING",params![scope.agent_id,scope.identity_incarnation])?;
    }
    tx.execute("INSERT INTO task_query_birth_cursors(agent_id,identity_incarnation,high_water) VALUES(?1,?2,0) ON CONFLICT(agent_id,identity_incarnation) DO NOTHING",params![scope.agent_id,scope.identity_incarnation])?;
    Ok(())
}

/// Called only from the original Task finalizer after actual readback/alias ACK.
/// Receipt replay exits the enclosing writer before reaching this function.
pub(super) fn register_actual_task_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    task: &TaskRecord,
    content: &str,
    projection: &str,
    first_native_insert: bool,
) -> Result<()> {
    let exact:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM task_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family='task' AND record_id=?3 AND availability='present' AND content_digest=?4 AND projection_digest=?5)",params![scope.agent_id,scope.identity_incarnation,task.id,content,projection],|row|row.get(0))?;
    ensure!(
        exact && task.agent_id == scope.agent_id,
        "Task query write lacks actual qualified Task ACK"
    );
    let reply = if task.kind == TaskKind::AgentMessageWait && task.status == TaskStatus::Completed {
        task.detail
            .as_ref()
            .and_then(|detail| detail.get("message_id"))
            .and_then(serde_json::Value::as_str)
    } else {
        None
    };
    tx.execute("INSERT INTO task_query_task_members(agent_id,identity_incarnation,task_id,content_digest,projection_digest,reply_message_id,created_at,parent_turn_id,task_status,header_digest) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(agent_id,identity_incarnation,task_id) DO UPDATE SET content_digest=excluded.content_digest,projection_digest=excluded.projection_digest,reply_message_id=excluded.reply_message_id,created_at=excluded.created_at,parent_turn_id=excluded.parent_turn_id,task_status=excluded.task_status,header_digest=excluded.header_digest",params![scope.agent_id,scope.identity_incarnation,task.id,content,projection,reply,repositories::timestamp(task.created_at),task.detail.as_ref().and_then(|d|d.get("parent_turn_id")).and_then(serde_json::Value::as_str),serde_json::to_value(&task.status)?.as_str().context("Task status encoding differs")?,super::monitor_relations::task_header_digest(task,content,projection)?])?;
    let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM task_query_task_births WHERE agent_id=?1 AND identity_incarnation=?2 AND task_id=?3)",params![scope.agent_id,scope.identity_incarnation,task.id],|row|row.get(0))?;
    if exists {
        return Ok(());
    }
    tx.execute("INSERT INTO task_query_birth_cursors(agent_id,identity_incarnation,high_water) VALUES(?1,?2,0) ON CONFLICT(agent_id,identity_incarnation) DO NOTHING",params![scope.agent_id,scope.identity_incarnation])?;
    let seq: u64 = if first_native_insert {
        tx.query_row("UPDATE task_query_birth_cursors SET high_water=high_water+1 WHERE agent_id=?1 AND identity_incarnation=?2 AND high_water<9223372036854775807 RETURNING high_water",params![scope.agent_id,scope.identity_incarnation],|row|row.get(0))?
    } else {
        0
    };
    tx.execute("INSERT INTO task_query_task_births(agent_id,identity_incarnation,task_id,birth_seq) VALUES(?1,?2,?3,?4)",params![scope.agent_id,scope.identity_incarnation,task.id,seq])?;
    Ok(())
}

pub(super) fn observe_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    query: &TaskQuerySelection,
    material: &mut read_preparation::TaskReadMaterial<'_>,
) -> Result<TaskQueryCoverage> {
    let complete:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM task_query_task_inventory WHERE agent_id=?1 AND identity_incarnation=?2 AND complete=1)",params![scope.agent_id,scope.identity_incarnation],|row|row.get(0))?;
    if !complete {
        ensure!(
            query.birth_after.is_none()
                && query.reply_message_ids.is_empty()
                && query.parent_turn_ids.is_empty(),
            TaskReadError::IncompleteQuery
        );
        return Ok(TaskQueryCoverage::default());
    }
    let cursor:u64=tx.query_row("SELECT high_water FROM task_query_birth_cursors WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation],|row|row.get(0))?;
    let mut result = TaskQueryCoverage {
        birth_cursor: Some(cursor),
        ..Default::default()
    };
    if let Some(after) = query.birth_after {
        ensure!(
            after <= cursor,
            "Task query birth cursor exceeds accepted high water"
        );
        let (count,bytes):(usize,usize)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(task_id AS BLOB))+256),0) FROM task_query_task_births WHERE agent_id=?1 AND identity_incarnation=?2 AND birth_seq>?3",params![scope.agent_id,scope.identity_incarnation,after],|row|Ok((row.get(0)?,row.get(1)?)))?;
        material.charge_ids(bytes, count)?;
        let mut statement=tx.prepare("SELECT task_id,birth_seq FROM task_query_task_births WHERE agent_id=?1 AND identity_incarnation=?2 AND birth_seq>?3 ORDER BY birth_seq,task_id")?;
        for row in statement.query_map(
            params![scope.agent_id, scope.identity_incarnation, after],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)),
        )? {
            let (id, seq) = row?;
            ensure!(
                !id.is_empty() && seq > after && seq <= cursor,
                "Task query birth row differs"
            );
            ensure!(
                result.born_task_seqs.insert(id, seq).is_none(),
                "Task query birth is duplicate"
            );
        }
        ensure!(
            result.born_task_seqs.len() == count,
            "Task query birth set changed within original view"
        );
        result.birth_after = Some(after);
    }
    for reply in &query.reply_message_ids {
        ensure!(!reply.is_empty(), "Task query reply selector is empty");
        let (count,bytes):(usize,usize)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(task_id AS BLOB))+256),0) FROM task_query_task_members WHERE agent_id=?1 AND identity_incarnation=?2 AND reply_message_id=?3",params![scope.agent_id,scope.identity_incarnation,reply],|row|Ok((row.get(0)?,row.get(1)?)))?;
        material.charge_ids(
            bytes
                .checked_add(reply.len() + 256)
                .context("Task reply selector bytes overflow")?,
            count.checked_add(1).context("Task reply count overflow")?,
        )?;
        let mut statement=tx.prepare("SELECT task_id FROM task_query_task_members WHERE agent_id=?1 AND identity_incarnation=?2 AND reply_message_id=?3 ORDER BY created_at,task_id")?;
        let ids = statement
            .query_map(
                params![scope.agent_id, scope.identity_incarnation, reply],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<BTreeSet<_>>>()?;
        ensure!(
            ids.len() == count && ids.iter().all(|id| !id.is_empty()),
            "Task reply relation changed within original view"
        );
        result.reply_task_ids.insert(reply.clone(), ids);
    }
    for turn in &query.parent_turn_ids {
        super::monitor_relations::observe_parent_turn_tx(tx, scope, turn, material, &mut result)?;
    }
    Ok(result)
}
