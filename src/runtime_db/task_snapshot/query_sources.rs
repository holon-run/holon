//! Original Message writers maintain this index; readers never backfill history.
use super::*;
use crate::{
    runtime::resident_input_evidence::{
        EvidenceLocator, PreparedInputEvidence, QualifiedInputEvidence,
    },
    types::{MessageKind, MessageOrigin},
};

// Both the fixed and current paths use received order metadata. A raw
// canonical update cannot move another accepted Source into the global window.
const HEADERS: &str = "SELECT h.evidence_id,h.message_seq,h.created_at,h.message_id FROM task_message_order_headers h WHERE h.agent_id=?1 AND h.identity_incarnation=?2";
fn recent_headers() -> String {
    format!("{HEADERS} ORDER BY message_seq IS NULL ASC,message_seq DESC,created_at DESC,message_id ASC LIMIT 200")
}

/// Fresh-empty bootstrap is the only non-fixed way to seal an empty inventory.
/// Existing canonical history needs the original fixed collector, not this API.
pub(super) fn initialize_fresh_source_inventory_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
) -> Result<()> {
    if tx.query_row("SELECT EXISTS(SELECT 1 FROM task_message_source_inventory WHERE agent_id=?1 AND identity_incarnation=?2)",params![scope.agent_id,scope.identity_incarnation],|r|r.get::<_,bool>(0))? {
        return Ok(());
    }
    let fresh:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2 AND policy_version=1 AND mode='fresh_empty' AND seed_id IS NULL) AND NOT EXISTS(SELECT 1 FROM messages m WHERE m.agent_id=?1 AND NOT EXISTS(SELECT 1 FROM input_evidence_current a WHERE a.agent_id=m.agent_id AND a.identity_incarnation=?2 AND a.family='message' AND a.record_id=m.evidence_id AND a.availability='present'))",params![scope.agent_id,scope.identity_incarnation],|r|r.get(0))?;
    if fresh {
        tx.execute("INSERT INTO task_message_source_inventory(agent_id,identity_incarnation,origin,seed_id) VALUES(?1,?2,'fresh_empty',NULL)",params![scope.agent_id,scope.identity_incarnation])?;
    }
    Ok(())
}

/// Original durable fixed members only. This never scans a late canonical tail
/// or grants Input body membership; current aliases remain the stronger fence.
pub(super) fn initialize_source_inventory_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    seed: Option<&str>,
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
            "Task source fixed inventory crossed agent"
        );
        let initialized = tx.execute("INSERT INTO task_message_source_inventory(agent_id,identity_incarnation,origin,seed_id,query_inventory_id) VALUES(?1,?2,'fixed',?3,?4) ON CONFLICT(agent_id,identity_incarnation) DO NOTHING",params![scope.agent_id,scope.identity_incarnation,seed,inventory.id])?;
        let matching:bool=tx.query_row("SELECT origin='fixed' AND seed_id IS ?3 AND query_inventory_id=?4 FROM task_message_source_inventory WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation,seed,inventory.id],|row|row.get(0))?;
        ensure!(matching, "Task source fixed inventory changed");
        // The immutable original members were copied atomically with this marker.
        // Later actual ACKs maintain their headers; preparation cannot refill them.
        if initialized == 0 {
            return Ok(());
        }
        tx.execute("INSERT INTO task_message_order_headers(agent_id,identity_incarnation,evidence_id,message_id,message_seq,created_at,revision,content_digest) SELECT ?1,?2,f.evidence_id,f.message_id,f.message_seq,f.created_at,CASE WHEN a.availability='present' AND a.content_digest=f.content_digest THEN a.revision ELSE 1 END,f.content_digest FROM task_query_fixed_messages f LEFT JOIN input_evidence_current a ON a.agent_id=?1 AND a.identity_incarnation=?2 AND a.family='message' AND a.record_id=f.evidence_id WHERE f.inventory_id=?3 ON CONFLICT(agent_id,identity_incarnation,evidence_id) DO NOTHING",params![scope.agent_id,scope.identity_incarnation,inventory.id])?;
        tx.execute("INSERT INTO task_message_sources(agent_id,identity_incarnation,source_id,task_id,is_result,message_status,source_locator_json) SELECT ?1,?2,f.evidence_id,f.source_task_id,f.is_result,f.message_status,json_object('scope',json_object('agent_id',?1,'identity_incarnation',?2),'key',json_object('family','message','id',f.evidence_id),'revision',CASE WHEN a.availability='present' AND a.content_digest=f.content_digest THEN a.revision ELSE 1 END,'content_digest',f.content_digest) FROM task_query_fixed_messages f LEFT JOIN input_evidence_current a ON a.agent_id=?1 AND a.identity_incarnation=?2 AND a.family='message' AND a.record_id=f.evidence_id WHERE f.inventory_id=?3 AND f.source_task_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM task_message_order_headers h WHERE h.agent_id=?1 AND h.identity_incarnation=?2 AND h.evidence_id=f.evidence_id AND h.content_digest<>f.content_digest) ON CONFLICT(agent_id,identity_incarnation,source_id) DO NOTHING",params![scope.agent_id,scope.identity_incarnation,inventory.id])?;
    } else {
        initialize_fresh_source_inventory_tx(tx, scope)?;
    }
    Ok(())
}

/// A historical unavailable table is not an observed empty namespace.
pub(super) fn source_inventory_complete_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
) -> Result<bool> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_message_source_inventory i WHERE i.agent_id=?1 AND i.identity_incarnation=?2 AND (i.origin='fresh_empty' OR EXISTS(SELECT 1 FROM task_query_fixed_manifests f WHERE f.inventory_id=i.query_inventory_id AND f.agent_id=i.agent_id AND f.sealed=1 AND f.message_observed=1)))",
        params![scope.agent_id,scope.identity_incarnation], |row| row.get(0),
    ).map_err(Into::into)
}

/// Called after actual readback and D3 alias mint in its original transaction.
/// This metadata never authorizes reading a source body: joint capture checks
/// the selected Message's exact qualified locator independently.
pub(in crate::runtime_db) fn register_message_source_tx(
    tx: &Transaction<'_>,
    record: &QualifiedInputEvidence,
) -> Result<()> {
    let PreparedInputEvidence::Message(message) = &record.record else {
        return Ok(());
    };
    ensure!(
        record.locator.key == InputEvidenceKey::new(InputEvidenceFamily::Message, &message.id)
            && record.locator.scope.agent_id == message.agent_id,
        "Task source differs from original Message ACK"
    );
    let received=tx.execute("INSERT INTO task_message_order_headers(agent_id,identity_incarnation,evidence_id,message_id,message_seq,created_at,revision,content_digest) SELECT ?1,?2,m.evidence_id,m.message_id,m.message_seq,m.created_at,?4,?5 FROM messages m JOIN input_evidence_current a ON a.agent_id=m.agent_id AND a.record_id=m.evidence_id AND a.identity_incarnation=?2 AND a.family='message' WHERE m.agent_id=?1 AND m.evidence_id=?3 AND a.availability='present' AND a.revision=?4 AND a.content_digest=?5 ON CONFLICT(agent_id,identity_incarnation,evidence_id) DO UPDATE SET message_id=excluded.message_id,message_seq=excluded.message_seq,created_at=excluded.created_at,revision=excluded.revision,content_digest=excluded.content_digest",params![record.locator.scope.agent_id,record.locator.scope.identity_incarnation,message.id,record.locator.revision,record.locator.content_digest])?;
    ensure!(
        received == 1,
        "Task order requires the actual qualified Message ACK"
    );
    // An actual canonical update can remove old Task metadata as well as add it.
    // Raw late rows are never discovered here or by a normal reader.
    let task_id = if matches!(message.origin, MessageOrigin::Task { .. }) {
        message
            .metadata
            .as_ref()
            .and_then(|v| v.get("task_id"))
            .and_then(serde_json::Value::as_str)
    } else {
        None
    };
    let Some(task_id) = task_id else {
        tx.execute("DELETE FROM task_message_sources WHERE agent_id=?1 AND identity_incarnation=?2 AND source_id=?3",params![record.locator.scope.agent_id,record.locator.scope.identity_incarnation,message.id])?;
        return Ok(());
    };
    let exact:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_current a JOIN messages m ON m.evidence_id=a.record_id AND m.agent_id=a.agent_id WHERE a.agent_id=?1 AND a.identity_incarnation=?2 AND a.family='message' AND a.record_id=?3 AND a.availability='present' AND a.revision=?4 AND a.content_digest=?5)",params![record.locator.scope.agent_id,record.locator.scope.identity_incarnation,message.id,record.locator.revision,record.locator.content_digest],|r|r.get(0))?;
    ensure!(
        exact,
        "Task source index requires original qualified Message ACK"
    );
    let status = crate::runtime::resident_task::task_output_status_from_message(
        message,
        message
            .metadata
            .as_ref()
            .context("Task Source metadata missing")?,
    );
    let status = serde_json::to_value(status)?;
    tx.execute("INSERT INTO task_message_sources(agent_id,identity_incarnation,source_id,task_id,is_result,message_status,source_locator_json) VALUES(?1,?2,?3,?4,?5,?8,json_object('scope',json_object('agent_id',?1,'identity_incarnation',?2),'key',json_object('family','message','id',?3),'revision',?6,'content_digest',?7)) ON CONFLICT(agent_id,identity_incarnation,source_id) DO UPDATE SET task_id=excluded.task_id,is_result=excluded.is_result,message_status=excluded.message_status,source_locator_json=excluded.source_locator_json",
        params![record.locator.scope.agent_id,record.locator.scope.identity_incarnation,message.id,task_id,message.kind==MessageKind::TaskResult,record.locator.revision,record.locator.content_digest,status.as_str().context("Task Source status is not a string")?])?;
    Ok(())
}

/// Finite normalized headers only. The original Task loader supplies actual
/// Host + combined Memory accounting through this callback before any String.
pub(super) fn observe_output_heads_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    tasks: &[&TaskFactLocator],
    mut charge: impl FnMut(usize, usize) -> Result<()>,
) -> Result<TaskOutputCoverage> {
    let complete = source_inventory_complete_tx(tx, scope)?;
    ensure!(complete, TaskSnapshotError::OutputSourcesIncomplete);
    let received_recent = recent_headers();
    // Keep received tombstones/version misses in the ordering pool. The exact
    // selected head carries availability; ordinary Task writers do not require
    // an unrelated Source body or promote another historical candidate.
    let (count,bytes):(usize,usize)=tx.query_row(&format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(evidence_id AS BLOB))+128),0) FROM ({received_recent})"),params![scope.agent_id,scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?)))?;
    charge(bytes, count)?;
    let mut statement = tx.prepare(&format!("SELECT evidence_id FROM ({received_recent})"))?;
    let message_ids = statement
        .query_map(params![scope.agent_id, scope.identity_incarnation], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        message_ids.len() == count,
        "Task output window changed in original read view"
    );
    let more: bool = tx.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM ({HEADERS}) LIMIT 1 OFFSET 200)"),
        params![scope.agent_id, scope.identity_incarnation],
        |r| r.get(0),
    )?;
    let mut coverage = TaskOutputCoverage {
        window: Some(TaskOutputWindowObservation {
            message_ids,
            is_exhaustive: !more,
        }),
        heads: BTreeMap::new(),
    };
    for task in tasks {
        ensure!(
            task.scope == *scope && task.key.family == TaskFamily::Task,
            "Task output selector crossed namespace"
        );
        let missing_order:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM task_message_sources s WHERE s.agent_id=?1 AND s.identity_incarnation=?2 AND s.task_id=?3 AND NOT EXISTS(SELECT 1 FROM task_message_order_headers h WHERE h.agent_id=s.agent_id AND h.identity_incarnation=s.identity_incarnation AND h.evidence_id=s.source_id))",params![scope.agent_id,scope.identity_incarnation,task.key.id],|row|row.get(0))?;
        ensure!(!missing_order, TaskSnapshotError::OutputSourcesIncomplete);
        let mut selected = None;
        for (recent, result, place) in [
            (true, true, TaskOutputSourcePlace::RecentResult),
            (true, false, TaskOutputSourcePlace::RecentOther),
            (false, true, TaskOutputSourcePlace::HistoricalResult),
            (false, false, TaskOutputSourcePlace::HistoricalOther),
        ] {
            let predicate = if recent {
                format!("m.evidence_id IN (SELECT evidence_id FROM ({received_recent}))")
            } else {
                "1".into()
            };
            let direction = if recent { "ASC" } else { "DESC" };
            let from=format!("FROM task_message_sources s JOIN ({HEADERS}) m ON m.evidence_id=s.source_id WHERE s.agent_id=?1 AND s.identity_incarnation=?2 AND s.task_id=?3 AND s.is_result=?4 AND {predicate} ORDER BY m.message_seq IS NULL ASC,m.message_seq DESC,m.created_at DESC,m.message_id {direction} LIMIT 1");
            let bytes:Option<usize>=tx.query_row(&format!("SELECT length(CAST(s.source_locator_json AS BLOB))+length(CAST(s.message_status AS BLOB))+length(CAST(s.source_id AS BLOB))+length(CAST(s.task_id AS BLOB))+COALESCE((SELECT length(CAST(a.content_digest AS BLOB))+length(CAST(a.availability AS BLOB)) FROM input_evidence_current a WHERE a.agent_id=?1 AND a.identity_incarnation=?2 AND a.family='message' AND a.record_id=s.source_id),0)+128 {from}"),params![scope.agent_id,scope.identity_incarnation,task.key.id,result],|r|r.get(0)).optional()?;
            if let Some(bytes) = bytes {
                charge(bytes, 1)?;
                let (source_id, raw, message_status): (String, String, String) = tx.query_row(
                    &format!("SELECT s.source_id,s.source_locator_json,s.message_status {from}"),
                    params![
                        scope.agent_id,
                        scope.identity_incarnation,
                        task.key.id,
                        result
                    ],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                let message: EvidenceLocator = serde_json::from_str(&raw)?;
                ensure!(
                    message.key.id == source_id
                        && message.scope == *scope
                        && message.key.family == InputEvidenceFamily::Message
                        && message.revision > 0
                        && valid_input_digest(&message.content_digest),
                    "Task output source locator corrupt"
                );
                let availability = source_availability_tx(tx, scope, &message)?;
                let message_status =
                    serde_json::from_value(serde_json::Value::String(message_status))?;
                selected = Some(TaskOutputSourceHead {
                    place,
                    message,
                    availability,
                    message_status,
                });
                break;
            }
        }
        charge(read_preparation::encoded_metadata_bytes(task)? + 128, 1)?;
        let observed = TaskOutputHeadObservation {
            task: (*task).clone(),
            source: selected,
        };
        observed.validate(task)?;
        ensure!(
            coverage
                .heads
                .insert(task.key.id.clone(), observed)
                .is_none(),
            "duplicate Task output selector"
        );
    }
    Ok(coverage)
}

fn source_availability_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    message: &EvidenceLocator,
) -> Result<TaskOutputSourceAvailability> {
    let (pruned,present,overridden,fixed,header):(bool,bool,bool,bool,bool)=tx.query_row("SELECT         EXISTS(SELECT 1 FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family='message' AND record_id=?3 AND availability='pruned') OR (NOT EXISTS(SELECT 1 FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family='message' AND record_id=?3) AND EXISTS(SELECT 1 FROM task_message_source_inventory i JOIN task_output_fixed_input_prunes p ON p.inventory_id=i.query_inventory_id AND p.family='message' JOIN task_query_fixed_messages f ON f.inventory_id=p.inventory_id AND f.evidence_id=p.record_id AND f.content_digest=p.content_digest WHERE i.agent_id=?1 AND i.identity_incarnation=?2 AND p.record_id=?3)),        EXISTS(SELECT 1 FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family='message' AND record_id=?3 AND availability='present' AND revision=?4 AND content_digest=?5),        EXISTS(SELECT 1 FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family='message' AND record_id=?3),        EXISTS(SELECT 1 FROM task_message_source_inventory i JOIN task_query_fixed_messages f ON f.inventory_id=i.query_inventory_id WHERE i.agent_id=?1 AND i.identity_incarnation=?2 AND f.evidence_id=?3 AND f.content_digest=?5), EXISTS(SELECT 1 FROM task_message_order_headers h WHERE h.agent_id=?1 AND h.identity_incarnation=?2 AND h.evidence_id=?3 AND h.revision=?4 AND h.content_digest=?5) ",params![scope.agent_id,scope.identity_incarnation,message.key.id,message.revision,message.content_digest],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))?;
    Ok(if pruned {
        TaskOutputSourceAvailability::Pruned
    } else if header && (present || (!overridden && fixed)) {
        TaskOutputSourceAvailability::Present
    } else {
        TaskOutputSourceAvailability::MissingVersion
    })
}

fn valid_input_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "query_sources_tests.rs"]
mod tests;
