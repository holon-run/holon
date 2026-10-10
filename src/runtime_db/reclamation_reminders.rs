//! Durable responsibility observations and ordinary internal-message outbox.
//! No model response is parsed and no age threshold deletes an identity.
use super::{migrations::timestamp, reclamation::*, RuntimeDb};
use crate::types::*;
use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CandidateFact {
    child: String,
    parent: String,
    incarnation: u64,
    evidence: String,
}
#[derive(Default)]
pub(crate) struct ReminderScan {
    pub cursor: Option<String>,
    pub observed: usize,
    pub changed: usize,
}
fn fact_tx(tx: &Transaction<'_>, child: &str) -> Result<Option<CandidateFact>> {
    if !retirable_child_tx(tx, child)? {
        return Ok(None);
    }
    let relations =
        super::agent_relations::canonical_relations_from_connection(tx, child)?.unwrap();
    let supervision = relations.supervision.unwrap();
    let incarnation = tx.query_row("SELECT COALESCE(json_extract(payload_json, '$.incarnation'), 1) FROM agent_identities WHERE agent_id = ?1", [child], |r| r.get(0))?;
    // Include the current edge, not creation lineage. A transfer resets observation.
    let evidence = serde_json::to_string(&(
        idle_evidence_tx(tx, child)?,
        supervision.supervision_id,
        supervision.supervisor_agent_id.clone(),
        supervision.revision,
        relations.durability.as_ref().map(|d| d.revision),
        relations.lifecycle_attachment.as_ref().map(|a| a.revision),
    ))?;
    Ok(Some(CandidateFact {
        child: child.into(),
        parent: supervision.supervisor_agent_id,
        incarnation,
        evidence,
    }))
}
fn parent_can_receive_tx(tx: &Transaction<'_>, parent: &str) -> Result<bool> {
    let payload: Option<String> = tx.query_row("SELECT s.payload_json FROM agent_states s JOIN agent_identities i USING(agent_id) WHERE i.agent_id = ?1 AND i.status = 'active'", [parent], |r| r.get(0)).optional()?;
    let Some(payload) = payload else {
        return Ok(false);
    };
    let state: AgentState = serde_json::from_str(&payload)?;
    if state.status == AgentStatus::Stopped
        || state
            .turn_budget
            .as_ref()
            .is_some_and(|b| state.turn_index.saturating_sub(b.run_start_turn_index) >= b.max_turns)
    {
        return Ok(false);
    }
    let relations = super::agent_relations::canonical_relations_from_connection(tx, parent)?;
    Ok(relations.is_some_and(|r| {
        r.resolution == AgentCanonicalResolution::Resolved
            && r.capability_policy
                .is_some_and(|p| p.allows(AgentCapabilityFamily::AgentCreation))
    }))
}

/// Transient "parent is busy" state. Unlike [`parent_can_receive_tx`], which is
/// a durable eligibility gate, this only defers a reminder so a running parent
/// is not interrupted. Durable retention state (open work items, timers) is not
/// "busy".
fn parent_busy_blocker_tx(tx: &Transaction<'_>, parent: &str) -> Result<Option<String>> {
    let payload: Option<String> = tx.query_row("SELECT s.payload_json FROM agent_states s JOIN agent_identities i USING(agent_id) WHERE i.agent_id = ?1 AND i.status = 'active'", [parent], |r| r.get(0)).optional()?;
    let Some(payload) = payload else {
        return Ok(Some("parent_unavailable".into()));
    };
    let state: AgentState = serde_json::from_str(&payload)?;
    if matches!(
        state.status,
        AgentStatus::Booting | AgentStatus::AwakeRunning | AgentStatus::AwaitingTask
    ) || state.current_run_id.is_some()
        || state.pending > 0
        || state.pending_wake_hint.is_some()
    {
        return Ok(Some("parent_running".into()));
    }
    for (reason, sql) in [
        ("parent_active_task", "SELECT EXISTS(SELECT 1 FROM tasks WHERE owner_agent_id = ?1 AND status IN ('queued','running','cancelling'))"),
        ("parent_queued_input", "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE agent_id = ?1 AND status IN ('queued','dequeued','interrupted'))"),
        ("parent_accepted_delivery", "SELECT EXISTS(SELECT 1 FROM agent_message_deliveries WHERE target_agent_id = ?1 AND state IN ('queued','dispatched'))"),
    ] {
        if tx.query_row(sql, [parent], |row| row.get::<_, bool>(0))? {
            return Ok(Some(reason.into()));
        }
    }
    Ok(None)
}
impl RuntimeDb {
    /// Bounded keyset fallback. Only changed facts produce observation writes.
    pub(crate) fn scan_subagent_cleanup(
        &self,
        after: Option<&str>,
        limit: usize,
        now: DateTime<Utc>,
        grace: chrono::Duration,
        reminders_enabled: bool,
    ) -> Result<ReminderScan> {
        self.transaction(|tx| {
            let children = {
                let mut stmt = tx.prepare("SELECT s.child_agent_id FROM agent_supervisions s JOIN agent_identities i ON \
                    i.agent_id = s.child_agent_id WHERE s.state IN ('active','cleanup_required') \
                    AND i.status = 'active' AND s.child_agent_id > ?1 ORDER BY s.child_agent_id \
                    LIMIT ?2")?;
                let rows = stmt.query_map(params![after.unwrap_or(""), limit], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            let mut outcome = ReminderScan {
                observed: children.len(), cursor: if children.len() == limit {
                    children.last().cloned()
                } else {
                    None
                }, ..Default::default()
            };
            let mut parents = std::collections::BTreeSet::new();
            for child in children {
                let Some(mut fact) = fact_tx(tx, &child)? else {
                    continue;
                };
                let parent_active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_identities WHERE agent_id = ?1 AND status = 'active')", [&fact.parent], |r| r.get(0))?;
                let orphaned = !parent_active;
                if orphaned {
                    super::agent_relations::transition_supervision_state_tx(tx, &child, AgentSupervisionState::CleanupRequired, 0, now)?;
                    fact = fact_tx(tx, &child)?.expect("supervision remains canonical after cleanup-required transition");
                }
                let blocker = idle_blocker_tx(tx, &child)?;
                let previous: Option<(String, Option<String>, bool)> = tx.query_row("SELECT evidence, blocker, orphaned FROM subagent_cleanup_observations WHERE child_agent_id = ?1", [&child], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
                match previous {
                    // Only identity/supervision/activity evidence or orphan status
                    // restarts the idle grace. A changed blocker is just a refreshed
                    // hint and must not push next_reminder_at out forever.
                    Some((evidence, previous_blocker, previous_orphaned))
                        if evidence == fact.evidence && previous_orphaned == orphaned =>
                    {
                        if previous_blocker != blocker {
                            tx.execute("UPDATE subagent_cleanup_observations SET blocker=?2 WHERE child_agent_id=?1", params![child, blocker])?;
                            outcome.changed += 1;
                        }
                    }
                    _ => {
                        tx.execute("INSERT INTO \
                            subagent_cleanup_observations(child_agent_id,parent_agent_id,evidence,observed_since,next_reminder_at,blocker,orphaned,notice_id) \
                            VALUES(?1,?2,?3,?4,?5,?6,?7,NULL) ON CONFLICT(child_agent_id) DO UPDATE SET \
                            parent_agent_id=excluded.parent_agent_id,evidence=excluded.evidence,observed_since=excluded.observed_since,next_reminder_at=excluded.next_reminder_at,blocker=excluded.blocker,orphaned=excluded.orphaned,notice_id=NULL", params![child, fact.parent, fact.evidence, timestamp(now), timestamp(now.checked_add_signed(grace).ok_or_else(||anyhow::anyhow!("idle_grace_seconds exceeds timestamp range"))?), blocker, orphaned])?;
                        outcome.changed += 1;
                    }
                }
                parents.insert(fact.parent);
            }
            if reminders_enabled {
                for parent in parents {
                    if !parent_can_receive_tx(tx, &parent)? {
                        continue;
                    }
                    if parent_busy_blocker_tx(tx, &parent)?.is_some() {
                        // Defer without settling: the pending outbox is retried
                        // once the parent is idle again.
                        continue;
                    }
                    let previous: Option<(String, String, String)> = tx.query_row("SELECT message_id,state,created_at FROM subagent_cleanup_outbox WHERE parent_agent_id=?1", [&parent], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
                    if let Some((id, state, created)) = previous {
                        if state == "pending" {
                            continue;
                        }
                        let pending: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM queue_entries WHERE message_id=?1 AND status IN ('queued','dequeued','interrupted'))", [id], |r| r.get(0))?;
                        if pending || created > timestamp(now - chrono::Duration::days(1)) {
                            continue;
                        }
                    }
                    let due = {
                        let mut stmt = tx.prepare("SELECT child_agent_id,evidence FROM subagent_cleanup_observations WHERE \
                            parent_agent_id=?1 AND orphaned=0 AND \
                            next_reminder_at<=?2 ORDER BY next_reminder_at,child_agent_id LIMIT ?3")?;
                        let rows=stmt.query_map(params![parent,timestamp(now),limit], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
                        rows
                    };
                    let mut facts = Vec::new();
                    let mut hints = std::collections::BTreeMap::new();
                    for (child, evidence) in due {
                        if let Some(fact) = fact_tx(tx, &child)? {
                            // Only live execution suppresses a reminder. Retention
                            // state is attached as a hint below.
                            if fact.parent == parent
                                && fact.evidence == evidence
                                && execution_blocker_tx(tx, &child)?.is_none()
                            {
                                hints.insert(child.clone(), idle_blocker_tx(tx, &child)?);
                                facts.push(fact);
                            }
                        }
                    }
                    if facts.is_empty() {
                        continue;
                    }
                    let list = facts.iter().map(|f| match hints.get(&f.child).and_then(|hint| hint.as_deref()) {
                        Some(reason) => format!("{} (incarnation {}, {})",f.child,f.incarnation,reason),
                        None => format!("{} (incarnation {})",f.child,f.incarnation),
                    }).collect::<Vec<_>>().join(", ");
                    let message = MessageEnvelope::new(&parent, MessageKind::InternalFollowup, MessageOrigin::System {
                        subsystem: "subagent_reclamation".into()
                    }, AuthorityClass::RuntimeInstruction, Priority::Background, MessageBody::Text {
                        text: format!("Retained subagents still under your supervision have been idle: {list}. \
                            Review whether they are still needed. You own cleanup: use GetAgent to \
                            inspect current identity and blockers, and DeleteAgent when safe and no \
                            longer needed. Idle state such as an open work item or timer is a hint, \
                            not a deletion requirement. Keep any needed agent. No special reply \
                            format is required; silence never authorizes deletion.")
                    }).with_admission(MessageDeliverySurface::RuntimeSystem, AdmissionContext::RuntimeOwned);
                    tx.execute("INSERT INTO \
                        subagent_cleanup_outbox(parent_agent_id,message_id,created_at,state,message_json,candidates_json) \
                        VALUES(?1,?2,?3,'pending',?4,?5) ON CONFLICT(parent_agent_id) DO UPDATE SET \
                        message_id=excluded.message_id,created_at=excluded.created_at,state=excluded.state,message_json=excluded.message_json,candidates_json=excluded.candidates_json", params![parent,message.id,timestamp(now),serde_json::to_string(&message)?,serde_json::to_string(&facts)?])?;
                    for fact in facts {
                        tx.execute("UPDATE subagent_cleanup_observations SET next_reminder_at=?2 WHERE child_agent_id=?1", params![fact.child,timestamp(now+chrono::Duration::days(1))])?;
                    }
                }
            }
            Ok(outcome)
        })
    }
    /// A durable outbox ID is reused across enqueue/ack crashes.
    pub(crate) fn pending_subagent_reminders(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MessageEnvelope>> {
        let connection = self.connection()?;
        let mut stmt = connection.prepare(
            "SELECT message_json FROM subagent_cleanup_outbox WHERE state='pending' AND \
            parent_agent_id NOT LIKE 'operator:%' AND parent_agent_id>?1 ORDER BY \
            parent_agent_id LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![after.unwrap_or(""), limit], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|s| Ok(serde_json::from_str(&s)?))
            .collect()
    }
    pub(crate) fn subagent_reminder_is_current(&self, message: &MessageEnvelope) -> Result<bool> {
        self.transaction(|tx| {
            if !parent_can_receive_tx(tx, &message.agent_id)? {
                return Ok(false);
            }
            if parent_busy_blocker_tx(tx, &message.agent_id)?.is_some() {
                // Defer, do not settle: the outbox is retried when the parent is idle.
                return Ok(false);
            }
            // An already durable queue admission is authoritative on replay.
            let queued: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM queue_entries WHERE message_id=?1)",[&message.id],|r| r.get(0))?;
            if queued {
                return Ok(true);
            }
            let json: Option<String>=tx.query_row("SELECT candidates_json FROM subagent_cleanup_outbox WHERE parent_agent_id=?1 AND message_id=?2 AND state='pending'",params![message.agent_id,message.id],|r| r.get(0)).optional()?;
            let Some(json)=json else {
                return Ok(false);
            };
            let facts: Vec<CandidateFact>=serde_json::from_str(&json)?;
            for fact in &facts {
                let current=fact_tx(tx,&fact.child)?;
                if current.as_ref().is_none_or(|c| c.parent != fact.parent || c.evidence != fact.evidence) || execution_blocker_tx(tx,&fact.child)?.is_some() {
                    tx.execute("UPDATE subagent_cleanup_outbox SET state='settled' WHERE message_id=?1",[&message.id])?;
                    return Ok(false);
                }
            }
            Ok(true)
        })
    }
    pub(crate) fn acknowledge_subagent_reminder(&self, id: &str) -> Result<()> {
        self.transaction(|tx| {
            tx.execute("UPDATE subagent_cleanup_outbox SET state='queued' WHERE message_id=?1 AND state='pending'",[id])?;
            Ok(())
        })
    }
    pub(crate) fn subagent_cleanup_candidates(
        &self,
        parent: &str,
        limit: usize,
    ) -> Result<Vec<SubagentCleanupCandidate>> {
        let connection = self.connection()?;
        let tx = connection.unchecked_transaction()?;
        let children = {
            let mut stmt = tx.prepare(
                "SELECT child_agent_id FROM agent_supervisions WHERE supervisor_agent_id=?1 \
                AND state IN ('active','cleanup_required') ORDER BY child_agent_id LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(params![parent, limit], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut candidates = Vec::new();
        for child in children {
            if let Some(fact) = fact_tx(&tx, &child)? {
                let observed_since=tx.query_row("SELECT observed_since FROM subagent_cleanup_observations WHERE child_agent_id=?1 AND evidence=?2",params![child,fact.evidence],|r| r.get(0)).optional()?;
                candidates.push(SubagentCleanupCandidate {
                    agent_id: child,
                    incarnation: fact.incarnation,
                    blocker: parent_cleanup_blocker_tx(&tx, &fact.child)?,
                    observed_since,
                });
            }
        }
        Ok(candidates)
    }
    /// Persist the exact operator brief before publication so retries keep its ID.
    pub(crate) fn orphan_cleanup_notice(
        &self,
        operator: &str,
        limit: usize,
    ) -> Result<Option<BriefRecord>> {
        self.transaction(|tx| {
            let existing: Option<String>=tx.query_row("SELECT message_json FROM subagent_cleanup_outbox WHERE parent_agent_id=?1 AND state='pending'",[format!("operator:{operator}")],|r| r.get(0)).optional()?;
            if let Some(json)=existing {
                return Ok(Some(serde_json::from_str(&json)?));
            }
            let children={
                let mut stmt=tx.prepare("SELECT o.child_agent_id,o.parent_agent_id FROM subagent_cleanup_observations \
                    o JOIN agent_identities i ON i.agent_id=o.child_agent_id WHERE o.orphaned=1 \
                    AND o.notice_id IS NULL AND i.status='active' ORDER BY child_agent_id LIMIT \
                    ?1")?;
                let rows=stmt.query_map([limit],|r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            let failures = {
                let mut stmt=tx.prepare("SELECT j.payload_json FROM agent_deletion_jobs j WHERE \
                    j.status='retryable_failed' AND \
                    json_extract(j.payload_json,'$.mode')='parent_cleanup' AND \
                    json_extract(j.payload_json,'$.attempts')>=3 AND NOT EXISTS(SELECT 1 FROM \
                    subagent_cleanup_failure_notices n WHERE n.notice_key=j.deletion_id || ':' \
                    || j.phase || ':' || \
                    COALESCE(json_extract(j.payload_json,'$.last_error'),'')) ORDER BY \
                    j.updated_at,j.deletion_id LIMIT ?1")?;
                let rows=stmt.query_map([limit],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                rows.into_iter().map(|json|serde_json::from_str::<AgentDeletionJob>(&json)).collect::<serde_json::Result<Vec<_>>>()?
            };
            if children.is_empty() && failures.is_empty(){
                return Ok(None);
            }
            let mut descriptions=children.iter().map(|(child,parent)|format!("{child} (parent {parent} unavailable)")).collect::<Vec<_>>();
            descriptions.extend(failures.iter().map(|j|format!("{} (cleanup {:?} repeatedly blocked: {})",j.agent_id,j.phase,j.last_error.as_deref().unwrap_or("unknown failure"))));
            let text=descriptions.join(", ");
            let brief=BriefRecord::new(operator,BriefKind::Ack,format!("Subagent cleanup requires operator attention: {text}. Unresolved supervision \
                or failed cleanup needs review through the authorized control plane. No \
                automatic age-based deletion is performed."),None,None);
            tx.execute("INSERT INTO \
                subagent_cleanup_outbox(parent_agent_id,message_id,created_at,state,message_json,candidates_json) \
                VALUES(?1,?2,?3,'pending',?4,'[]') ON CONFLICT(parent_agent_id) DO UPDATE \
                SET \
                message_id=excluded.message_id,created_at=excluded.created_at,state=excluded.state,message_json=excluded.message_json",params![format!("operator:{operator}"),brief.id,timestamp(brief.created_at),serde_json::to_string(&brief)?])?;
            for(child,_)in children {
                tx.execute("UPDATE subagent_cleanup_observations SET notice_id=?2 WHERE child_agent_id=?1",params![child,brief.id])?;
            }
            for job in failures {
                let phase=serde_json::to_value(job.phase)?.as_str().unwrap().to_string();
                let key=format!("{}:{}:{}",job.deletion_id,phase,job.last_error.as_deref().unwrap_or(""));
                tx.execute("INSERT OR IGNORE INTO subagent_cleanup_failure_notices(notice_key,brief_id) VALUES(?1,?2)",params![key,brief.id])?;
            }
            Ok(Some(brief))
        })
    }
}
