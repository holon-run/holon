//! Fault observations and durable failure publication, never recovery authority.

use super::*;
use crate::domain::execution_protocol::{ExecutionAttempt, WorkItemExecutionRecord};
use crate::types::{
    AgentIdentityRecord, AuthorityClass, BriefKind, BriefRecord, MessageEnvelope, MessageOrigin,
    WorkItemRecord, WorkItemState,
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct SourceRevisionDrift {
    pub work_item_id: String,
    pub generation: u64,
    pub source_revision: u64,
    pub durable_revision: u64,
}

impl SourceRevisionDrift {
    fn signature(&self) -> Result<String> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)))
    }
}

#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct SourceRevisionRepairAssessment {
    pub episode_id: String,
    pub incarnation: u64,
    pub drift: SourceRevisionDrift,
    /// Computed from current canonical records, not the cached episode flag.
    pub drift_present: bool,
    pub first_message_id: String,
    pub first_attempt_id: Option<String>,
    pub rejection: String,
    pub alert_brief_id: String,
    pub alert_created_event_seq: Option<u64>,
    pub retained_source_count: u64,
    /// Disappearance of drift does not authorize replay of a retained source.
    pub retained_source_disposition: String,
}

fn incarnation(connection: &Connection, agent_id: &str) -> Result<Option<u64>> {
    let payload: Option<String> = connection
        .query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
            [agent_id],
            |row| row.get(0),
        )
        .optional()?;
    payload
        .map(|payload| {
            let identity: AgentIdentityRecord = serde_json::from_str(&payload)?;
            Ok(identity.incarnation)
        })
        .transpose()
}

pub(crate) fn source_revision_repair_retains_source_tx(
    connection: &Connection,
    agent_id: &str,
    message_id: &str,
) -> Result<bool> {
    let Some(incarnation) = incarnation(connection, agent_id)? else {
        return Ok(false);
    };
    // Resolution clears the fault, not the retained source's consumption fence.
    Ok(connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM source_revision_repair_block_sources
             WHERE agent_id = ?1 AND incarnation = ?2 AND message_id = ?3
         )",
        params![agent_id, incarnation, message_id],
        |row| row.get(0),
    )?)
}

pub(super) fn current_source_revision_drifts(
    connection: &Connection,
    agent_id: &str,
) -> Result<Vec<SourceRevisionDrift>> {
    let mut statement = connection.prepare(
        "SELECT w.work_item_id, w.payload_json, e.payload_json
         FROM work_items w JOIN execution_protocol_work_items e
           ON e.agent_id = w.agent_id AND e.work_item_id = w.work_item_id
         WHERE w.agent_id = ?1 ORDER BY w.work_item_id",
    )?;
    let rows = statement
        .query_map([agent_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut drifts = Vec::new();
    for (work_item_id, work, execution) in rows {
        let Ok(work) = serde_json::from_str::<WorkItemRecord>(&work) else {
            continue;
        };
        let Ok(execution) = serde_json::from_str::<WorkItemExecutionRecord>(&execution) else {
            continue;
        };
        if work.state == WorkItemState::Open && work.revision != execution.source_revision {
            drifts.push(SourceRevisionDrift {
                work_item_id,
                generation: execution.generation(),
                source_revision: execution.source_revision,
                durable_revision: work.revision,
            });
        }
    }
    Ok(drifts)
}

fn queue_publication(
    tx: &Transaction<'_>,
    episode_id: &str,
    publication_id: &str,
    agent_id: &str,
    message_id: &str,
    text: String,
    now: DateTime<Utc>,
) -> Result<()> {
    let mut brief = BriefRecord::new(
        agent_id,
        BriefKind::Failure,
        text,
        Some(message_id.into()),
        None,
    );
    brief.id = publication_id.into();
    brief.created_at = now;
    tx.execute(
        "INSERT OR IGNORE INTO source_revision_repair_block_publications
           (publication_id, episode_id, brief_json) VALUES (?1, ?2, ?3)",
        params![publication_id, episode_id, serde_json::to_string(&brief)?],
    )?;
    Ok(())
}

/// Expectations are read and compared in the caller's writer transaction.
/// Resolving an observation writes no execution/queue/wait/consumption records.
pub(super) fn observe_source_revision_repair_blocks_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    message_id: &str,
    attempt: Option<&ExecutionAttempt>,
    rejection: &str,
    now: DateTime<Utc>,
) -> Result<Vec<String>> {
    let Some(incarnation) = incarnation(tx, agent_id)? else {
        return Ok(Vec::new());
    };
    let drifts = current_source_revision_drifts(tx, agent_id)?;
    let active = drifts
        .iter()
        .map(|drift| Ok((drift, drift.signature()?)))
        .collect::<Result<Vec<_>>>()?;
    let previous = {
        let mut statement = tx.prepare(
            "SELECT episode_id, incarnation, signature FROM source_revision_repair_blocks
             WHERE agent_id = ?1 AND resolved_at IS NULL",
        )?;
        let rows = statement.query_map([agent_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (id, prior_incarnation, signature) in previous {
        if prior_incarnation != incarnation || !active.iter().any(|(_, key)| key == &signature) {
            tx.execute(
                "UPDATE source_revision_repair_blocks SET resolved_at = ?2
                 WHERE episode_id = ?1 AND resolved_at IS NULL",
                params![id, timestamp(now)],
            )?;
        }
    }
    let mut episodes = Vec::new();
    for (drift, signature) in active {
        let existing: Option<String> = tx
            .query_row(
                "SELECT episode_id FROM source_revision_repair_blocks
                 WHERE agent_id = ?1 AND incarnation = ?2 AND signature = ?3
                   AND resolved_at IS NULL",
                params![agent_id, incarnation, signature],
                |row| row.get(0),
            )
            .optional()?;
        let id = existing.unwrap_or_else(|| format!("repair-block:{}", uuid::Uuid::new_v4()));
        tx.execute(
            "INSERT OR IGNORE INTO source_revision_repair_blocks (
               episode_id, agent_id, incarnation, signature, work_item_id, generation,
               source_revision, durable_revision, first_message_id, first_attempt_id,
               first_source_json, rejection, opened_at, last_observed_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
            params![
                id,
                agent_id,
                incarnation,
                signature,
                drift.work_item_id,
                drift.generation,
                drift.source_revision,
                drift.durable_revision,
                message_id,
                attempt.map(|attempt| &attempt.attempt_id),
                attempt
                    .map(|attempt| serde_json::to_string(&attempt.source))
                    .transpose()?,
                rejection,
                timestamp(now),
            ],
        )?;
        tx.execute(
            "UPDATE source_revision_repair_blocks SET last_observed_at = ?2
             WHERE episode_id = ?1",
            params![id, timestamp(now)],
        )?;
        queue_publication(
            tx,
            &id,
            &format!("{id}:alert"),
            agent_id,
            message_id,
            format!(
                "Local source revision fault blocks execution: WorkItem {} has source revision {} \
                 but durable revision {} (generation {}). Repair episode: {id}. \
                 Inspect scheduler-repair diagnostics; retained inputs are not automatically replayed.",
                drift.work_item_id, drift.source_revision, drift.durable_revision, drift.generation,
            ),
            now,
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO source_revision_repair_block_sources
               (agent_id, incarnation, message_id, episode_id) VALUES (?1, ?2, ?3, ?4)",
            params![agent_id, incarnation, message_id, id],
        )?;
        episodes.push(id);
    }
    Ok(episodes)
}

impl RuntimeDb {
    pub(crate) fn source_revision_repair_retains_source(
        &self,
        agent_id: &str,
        message_id: &str,
    ) -> Result<bool> {
        self.with_read(|connection| {
            source_revision_repair_retains_source_tx(connection, agent_id, message_id)
        })
    }

    pub fn source_revision_drifts(&self, agent_id: &str) -> Result<Vec<SourceRevisionDrift>> {
        self.with_read(|connection| current_source_revision_drifts(connection, agent_id))
    }

    /// Scheduler selection suppression only. Never claims, consumes, restores, or
    /// requeues the source. Fresh operator inputs get a durable per-input receipt.
    pub fn source_revision_repair_blocks_source(
        &self,
        message: &MessageEnvelope,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        self.transaction(|tx| {
            let episodes = observe_source_revision_repair_blocks_tx(
                tx, &message.agent_id, &message.id, None, "scheduler_source_revision_drift", now,
            )?;
            let Some(incarnation) = incarnation(tx, &message.agent_id)? else {
                return Ok(false);
            };
            let retained: Option<String> = tx
                .query_row(
                    "SELECT episode_id FROM source_revision_repair_block_sources
                     WHERE agent_id = ?1 AND incarnation = ?2 AND message_id = ?3
                     ORDER BY episode_id LIMIT 1",
                    params![message.agent_id, incarnation, message.id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(episode_id) = retained else {
                return Ok(false);
            };
            if message.authority_class == AuthorityClass::OperatorInstruction
                && matches!(message.origin, MessageOrigin::Operator { .. })
                && matches!(
                    message.admission_context,
                    Some(
                        crate::types::AdmissionContext::ControlAuthenticated
                            | crate::types::AdmissionContext::OperatorTransportAuthenticated
                            | crate::types::AdmissionContext::LocalProcess
                    )
                )
            {
                let key = format!("{}:{incarnation}:{}", message.agent_id, message.id);
                let id = format!("repair-receipt:{:x}", Sha256::digest(key.as_bytes()));
                let reason = if episodes.is_empty() {
                    "The revision fault is no longer present, but this retained input has no \
                     automatic replay authority. Submit a new authorized input after reviewing diagnostics."
                } else {
                    "A local source revision fault requires repair."
                };
                queue_publication(
                    tx, &episode_id, &id, &message.agent_id, &message.id,
                    format!(
                        "Received input {}; this input was not executed. {reason} \
                         Repair episode: {episode_id}. Inspect scheduler-repair diagnostics.",
                        message.id,
                    ),
                    now,
                )?;
            }
            Ok(true)
        })
    }

    pub fn pending_source_revision_repair_briefs(
        &self,
        agent_id: &str,
    ) -> Result<Vec<BriefRecord>> {
        self.with_read(|connection| {
            let Some(incarnation) = incarnation(connection, agent_id)? else {
                return Ok(Vec::new());
            };
            let mut statement = connection.prepare(
                "SELECT p.brief_json FROM source_revision_repair_block_publications p
                 JOIN source_revision_repair_blocks b ON b.episode_id = p.episode_id
                 WHERE b.agent_id = ?1 AND b.incarnation = ?2 AND p.created_event_seq IS NULL
                 ORDER BY b.opened_at, p.publication_id",
            )?;
            let payloads = statement
                .query_map(params![agent_id, incarnation], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            payloads
                .into_iter()
                .map(|json| Ok(serde_json::from_str(&json)?))
                .collect()
        })
    }

    /// Revalidate the publication and incarnation in the same transaction as
    /// the Brief/event, index outbox, and publication linkage.
    pub(crate) fn commit_source_revision_repair_brief(
        &self,
        brief: &BriefRecord,
        changes: &[crate::runtime_db::RuntimeIndexChange],
    ) -> Result<Option<crate::runtime_db::evidence::BriefCreatedCommit>> {
        self.transaction(|tx| {
            let expected: Option<(u64, String)> = tx
                .query_row(
                    "SELECT b.incarnation, p.brief_json
                     FROM source_revision_repair_block_publications p
                     JOIN source_revision_repair_blocks b ON b.episode_id = p.episode_id
                     WHERE p.publication_id = ?1 AND b.agent_id = ?2",
                    params![brief.id, brief.agent_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((expected_incarnation, payload)) = expected else {
                return Ok(None);
            };
            if incarnation(tx, &brief.agent_id)? != Some(expected_incarnation) {
                return Ok(None);
            }
            let expected: BriefRecord = serde_json::from_str(&payload)?;
            ensure!(expected == *brief, "repair publication payload changed");
            let commit = crate::runtime_db::evidence::append_brief_with_created_event_tx(
                tx,
                Some(&brief.agent_id),
                brief,
                &crate::types::brief_created_event_for(brief)?,
                changes,
            )?;
            tx.execute(
                "UPDATE source_revision_repair_block_publications SET created_event_seq = ?2
                 WHERE publication_id = ?1 AND created_event_seq IS NULL",
                params![brief.id, commit.event.event_seq],
            )?;
            Ok(Some(commit))
        })
    }

    /// Same canonical predicate as the admission block, without mutation.
    pub fn source_revision_repair_assessments(
        &self,
        agent_id: &str,
    ) -> Result<Vec<SourceRevisionRepairAssessment>> {
        self.with_read(|connection| {
            let tx = connection.unchecked_transaction()?;
            let current_incarnation = incarnation(&tx, agent_id)?;
            let drifts = current_source_revision_drifts(&tx, agent_id)?;
            let mut statement = tx.prepare(
                "SELECT b.episode_id, b.incarnation, b.work_item_id, b.generation,
                        b.source_revision, b.durable_revision, b.first_message_id,
                        b.first_attempt_id, b.rejection, p.created_event_seq,
                        (SELECT COUNT(*) FROM source_revision_repair_block_sources s
                         WHERE s.episode_id = b.episode_id), b.resolved_at
                 FROM source_revision_repair_blocks b
                 LEFT JOIN source_revision_repair_block_publications p
                   ON p.publication_id = b.episode_id || ':alert'
                 WHERE b.agent_id = ?1 ORDER BY b.opened_at, b.episode_id",
            )?;
            let assessments = statement
                .query_map([agent_id], |row| {
                    let episode_id: String = row.get(0)?;
                    let incarnation = row.get(1)?;
                    let drift = SourceRevisionDrift {
                        work_item_id: row.get(2)?,
                        generation: row.get(3)?,
                        source_revision: row.get(4)?,
                        durable_revision: row.get(5)?,
                    };
                    let present = row.get::<_, Option<String>>(11)?.is_none()
                        && current_incarnation == Some(incarnation)
                        && drifts.contains(&drift);
                    Ok(SourceRevisionRepairAssessment {
                        alert_brief_id: format!("{episode_id}:alert"),
                        episode_id,
                        incarnation,
                        drift,
                        drift_present: present,
                        first_message_id: row.get(6)?,
                        first_attempt_id: row.get(7)?,
                        rejection: row.get(8)?,
                        alert_created_event_seq: row.get(9)?,
                        retained_source_count: row.get(10)?,
                        retained_source_disposition: if present {
                            "blocked_source_revision_drift"
                        } else {
                            "drift_absent_retained_source_requires_explicit_recovery"
                        }
                        .into(),
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(assessments)
        })
    }
}
