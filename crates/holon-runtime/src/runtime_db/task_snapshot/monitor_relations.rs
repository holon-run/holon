//! Complete parent-Turn status headers; neither canonical tails nor Task bodies.
use super::*;
use crate::types::{TaskRecord, TaskStatus};

pub(in crate::runtime_db) fn task_header_digest(
    task: &TaskRecord,
    content: &str,
    projection: &str,
) -> Result<String> {
    let parent = task
        .detail
        .as_ref()
        .and_then(|d| d.get("parent_turn_id"))
        .and_then(serde_json::Value::as_str);
    header_digest(&task.id, parent, &task.status, content, projection)
}
fn header_digest(
    id: &str,
    parent: Option<&str>,
    status: &TaskStatus,
    content: &str,
    projection: &str,
) -> Result<String> {
    super::super::input_mutation_receipts::canonical_command_digest(&(
        id, parent, status, content, projection,
    ))
}
const PARENT_ROWS: &str = "SELECT m.task_id,m.parent_turn_id,m.task_status,m.content_digest,m.projection_digest,m.header_digest,
 COALESCE(a.revision,1), CASE WHEN a.availability='pruned' THEN 'pruned'
 WHEN a.availability='present' AND a.content_digest=m.content_digest AND a.projection_digest=m.projection_digest THEN 'present'
 WHEN a.record_id IS NULL AND EXISTS(SELECT 1 FROM task_query_task_inventory i JOIN task_query_fixed_tasks f ON f.inventory_id=i.query_inventory_id AND f.task_id=m.task_id
 WHERE i.agent_id=m.agent_id AND i.identity_incarnation=m.identity_incarnation AND i.complete=1 AND f.content_digest=m.content_digest AND f.projection_digest=m.projection_digest AND f.header_digest=m.header_digest) THEN 'present'
 ELSE 'absent' END
 FROM task_query_task_members m LEFT JOIN task_domain_records a ON a.agent_id=m.agent_id AND a.identity_incarnation=m.identity_incarnation AND a.family='task' AND a.record_id=m.task_id
 WHERE m.agent_id=?1 AND m.identity_incarnation=?2 AND m.parent_turn_id=?3 ORDER BY m.task_id";

pub(super) fn observe_parent_turn_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    turn: &str,
    material: &mut read_preparation::TaskReadMaterial<'_>,
    result: &mut TaskQueryCoverage,
) -> Result<()> {
    ensure!(!turn.is_empty(), "parent Turn Task selector is empty");
    let (count,bytes):(usize,usize)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(task_id AS BLOB))+length(CAST(parent_turn_id AS BLOB))+length(CAST(task_status AS BLOB))+length(CAST(content_digest AS BLOB))+length(CAST(projection_digest AS BLOB))+COALESCE(length(CAST(header_digest AS BLOB)),0)+length(CAST(agent_id AS BLOB))+512),0) FROM task_query_task_members WHERE agent_id=?1 AND identity_incarnation=?2 AND parent_turn_id=?3",params![scope.agent_id,scope.identity_incarnation,turn],|row|Ok((row.get(0)?,row.get(1)?)))?;
    // Four retained copies plus the maximum JSON string escaping, before IDs,
    // status/digest strings, map nodes or scope copies are acquired.
    material.charge_ids(
        bytes
            .checked_mul(8)
            .and_then(|n| n.checked_add(turn.len() + 256))
            .context("parent Turn header bytes overflow")?,
        count
            .checked_mul(14)
            .and_then(|n| n.checked_add(1))
            .context("parent Turn header records overflow")?,
    )?;
    let mut headers = BTreeMap::new();
    let mut query = tx.prepare(PARENT_ROWS)?;
    let mut rows = query.query(params![scope.agent_id, scope.identity_incarnation, turn])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let parent: String = row.get(1)?;
        let status: String = row.get(2)?;
        let content: String = row.get(3)?;
        let projection: String = row.get(4)?;
        let accepted: Option<String> = row.get(5)?;
        let revision: u64 = row.get(6)?;
        let availability: String = row.get(7)?;
        ensure!(
            !id.is_empty() && parent == turn && revision > 0,
            "parent Turn Task metadata key/revision differs"
        );
        ensure!(
            content.len() == 64
                && projection.len() == 64
                && content
                    .bytes()
                    .chain(projection.bytes())
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "parent Turn Task digest differs"
        );
        let status: TaskStatus = serde_json::from_value(serde_json::Value::String(status))?;
        let availability = if accepted.as_deref()
            != Some(header_digest(&id, Some(&parent), &status, &content, &projection)?.as_str())
        {
            TaskAvailability::Absent
        } else {
            match availability.as_str() {
                "present" => TaskAvailability::Present,
                "pruned" => TaskAvailability::Pruned,
                _ => TaskAvailability::Absent,
            }
        };
        let header = TaskParentTurnHeader {
            task: TaskFactLocator {
                scope: scope.clone(),
                key: TaskKey::new(TaskFamily::Task, &id),
                revision,
                content_digest: content,
                projection_digest: projection,
            },
            parent_turn_id: parent,
            status,
            availability,
        };
        ensure!(
            headers.insert(id, header).is_none(),
            "duplicate parent Turn Task header"
        );
    }
    ensure!(
        headers.len() == count,
        "parent Turn Task relation changed within original view"
    );
    result.parent_turn_tasks.insert(turn.to_owned(), headers);
    Ok(())
}
