//! One-time Current adoption from the original Episode/Input upgrade cut.
use super::*;
use crate::{
    runtime::resident_input_evidence::InputEvidenceScope,
    types::{MessageEnvelope, QueueEntryRecord, QueueEntryStatus},
};

const SCOPES: &str = "FROM agent_episode_continuity c
    JOIN episode_continuity_seeds s ON s.seed_id=c.seed_id
    JOIN agent_identities i ON i.agent_id=c.agent_id
    WHERE c.mode='legacy_adopted' AND c.policy_version=1
      AND s.source_kind='upgrade_identity' AND i.status='active'";

pub(in crate::runtime_db) fn capture_tx(tx: &Transaction<'_>) -> Result<()> {
    use crate::runtime_db::current_read_writes::VERSION;
    let installed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version>=?1)
          OR EXISTS(SELECT 1 FROM current_read_qualification)
          OR EXISTS(SELECT 1 FROM current_read_revisions)",
        [VERSION],
        |row| row.get(0),
    )?;
    ensure!(
        !installed,
        "Current upgrade is not an original schema transaction"
    );
    let (count, bytes): (usize, usize) = tx.query_row(
        &format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(c.agent_id AS BLOB))+length(CAST(c.seed_id AS BLOB))+length(CAST(s.roots_json AS BLOB))+length(CAST(i.payload_json AS BLOB))+length(CAST(s.boundary_id AS BLOB))+length(CAST(s.identity_digest AS BLOB))),0) {SCOPES}"),
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        count <= CONTINUITY_MAX_RECORDS && bytes <= CONTINUITY_MAX_PAYLOAD_BYTES,
        "Current upgrade scope inventory exceeds limit"
    );
    let mut statement = tx.prepare(&format!(
        "SELECT c.agent_id,c.identity_incarnation,c.seed_id,s.agent_id,
                s.identity_incarnation,s.boundary_id,s.identity_digest,s.roots_json,
                i.payload_json,s.import_required {SCOPES} ORDER BY c.agent_id"
    ))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let scope = InputEvidenceScope {
            agent_id: row.get(0)?,
            identity_incarnation: row.get(1)?,
        };
        let seed = row.get_ref(2)?.as_str()?;
        ensure!(
            scope.identity_incarnation > 0
                && row.get_ref(3)?.as_str()? == scope.agent_id
                && row.get::<_, Option<u64>>(4)? == Some(scope.identity_incarnation)
                && !row.get::<_, bool>(9)?,
            "Current upgrade seed scope differs"
        );
        let boundary: String = tx.query_row(
            "SELECT value FROM runtime_metadata WHERE key=?1",
            [UPGRADE_BOUNDARY_KEY],
            |row| row.get(0),
        )?;
        ensure!(
            row.get_ref(5)?.as_str()? == boundary,
            "Current upgrade boundary differs"
        );
        let identity: AgentIdentityRecord = serde_json::from_str(row.get_ref(8)?.as_str()?)?;
        ensure!(
            identity.agent_id == scope.agent_id
                && identity.incarnation == scope.identity_incarnation
                && identity.status == AgentRegistryStatus::Active
                && std::str::from_utf8(&seed_identity_digest(&identity)?.0)?
                    == row.get_ref(6)?.as_str()?,
            "Current upgrade identity differs"
        );
        let roots: EpisodeContinuityRoots = serde_json::from_str(row.get_ref(7)?.as_str()?)?;
        ensure!(
            roots.record_digests.len() <= CONTINUITY_MAX_RECORDS,
            "Current upgrade record inventory exceeds limit"
        );
        let mut body_bytes = 0usize;
        let mut revision_inserted = false;
        for (key, frozen) in &roots.record_digests {
            let Some(id) = key.strip_prefix("queue:") else {
                continue;
            };
            let mut queue_statement = tx.prepare(
                "SELECT agent_id,priority,status,created_at,updated_at,payload_json
                 FROM queue_entries WHERE message_id=?1",
            )?;
            let mut queue_rows = queue_statement.query([id])?;
            let queue_row = queue_rows
                .next()?
                .context("Current upgrade Queue is absent")?;
            let payload = queue_row.get_ref(5)?.as_str()?;
            body_bytes = body_bytes
                .checked_add(payload.len())
                .context("Current upgrade bytes overflow")?;
            ensure!(
                body_bytes <= CONTINUITY_MAX_PAYLOAD_BYTES
                    && bytes_digest(payload.as_bytes()) == *frozen,
                "Current upgrade Queue fixed body changed"
            );
            let queue: QueueEntryRecord = serde_json::from_str(payload)?;
            ensure!(
                queue.message_id == id
                    && queue.agent_id == scope.agent_id
                    && queue_row.get_ref(0)?.as_str()? == scope.agent_id
                    && serde_json::to_value(&queue.priority)?.as_str()
                        == Some(queue_row.get_ref(1)?.as_str()?)
                    && serde_json::to_value(&queue.status)?.as_str()
                        == Some(queue_row.get_ref(2)?.as_str()?)
                    && crate::runtime_db::repositories::timestamp(queue.created_at)
                        == queue_row.get_ref(3)?.as_str()?
                    && crate::runtime_db::repositories::timestamp(queue.updated_at)
                        == queue_row.get_ref(4)?.as_str()?,
                "Current upgrade Queue canonical columns differ"
            );
            if !matches!(
                queue.status,
                QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
            ) {
                continue;
            }
            let mut message_statement = tx.prepare(
                "SELECT message_id,payload_json FROM messages WHERE agent_id=?1 AND evidence_id=?2",
            )?;
            let mut message_rows = message_statement.query(params![scope.agent_id, id])?;
            let message_row = message_rows
                .next()?
                .context("Current upgrade Message is absent")?;
            let message_payload = message_row.get_ref(1)?.as_str()?;
            body_bytes = body_bytes
                .checked_add(message_payload.len())
                .context("Current upgrade bytes overflow")?;
            ensure!(
                body_bytes <= CONTINUITY_MAX_PAYLOAD_BYTES
                    && roots
                        .record_digests
                        .get(&format!("message:{id}"))
                        .is_some_and(|digest| *digest == bytes_digest(message_payload.as_bytes())),
                "Current upgrade Message fixed body changed"
            );
            let message: MessageEnvelope = serde_json::from_str(message_payload)?;
            ensure!(
                message.id == id
                    && message.agent_id == scope.agent_id
                    && message_row.get_ref(0)?.as_str()? == id
                    && queue.priority == message.priority
                    && queue.created_at == message.created_at,
                "Current upgrade Message/Queue address differs"
            );
            crate::runtime_db::input_evidence_snapshot::validate_fixed_upgrade_message_tx(
                tx, &scope, seed, id,
            )?;
            if !revision_inserted {
                tx.execute(
                    "INSERT INTO current_read_revisions(agent_id,identity_incarnation,revision) VALUES(?1,?2,1)",
                    params![scope.agent_id, scope.identity_incarnation],
                )?;
                revision_inserted = true;
            }
            tx.execute(
                "INSERT INTO current_read_qualification(agent_id,identity_incarnation,family,record_id,revision,qualification_state,content_digest)
                 VALUES(?1,?2,'queue',?3,1,'qualified',?4)",
                params![scope.agent_id, scope.identity_incarnation, id, frozen],
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "current_read_upgrade_tests.rs"]
mod tests;
