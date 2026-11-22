//! The operator inbox projects committed facts, not agent-authored text.

use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use schemars::JsonSchema;
use serde::Serialize;

use crate::types::{BriefRecord, MessageEnvelope, MessageOrigin, WakeSource};

#[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NotificationKind {
    OperatorFirstBrief,
    WorkItemCompleted,
    OperatorInputRequired,
}

pub(super) fn record_work_owner(
    conn: &Connection,
    work_item_id: &str,
    principal: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO operator_inbox_work_owners (work_item_id, principal_id) VALUES (?1, ?2)",
        params![work_item_id, principal],
    )?;
    Ok(())
}

/// Only called inside the transaction publishing the canonical brief. Claim
/// each cause even when a higher-priority cause wins the display entry.
pub(super) fn project_brief(
    conn: &Connection,
    brief: &BriefRecord,
    wait_ids: &[&str],
) -> Result<()> {
    if !brief.kind.is_terminal() {
        return Ok(());
    }
    if let Some(input_id) = brief.related_message_id.as_deref() {
        let payload: Option<String> = conn
            .query_row(
                "SELECT payload_json FROM messages WHERE evidence_id = ?1 AND agent_id = ?2",
                params![input_id, brief.agent_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(payload) = payload {
            let input: MessageEnvelope = serde_json::from_str(&payload)?;
            if let Some(principal) = trusted_operator_principal(&input) {
                record_cause(
                    conn,
                    brief,
                    principal,
                    NotificationKind::OperatorFirstBrief,
                    input_id,
                    brief.work_item_id.as_deref(),
                )?;
            }
        }
    }
    if let Some(work_id) = brief.work_item_id.as_deref() {
        let principal: Option<String> = conn
            .query_row(
                "SELECT o.principal_id FROM operator_inbox_work_owners o
                 JOIN work_items w ON w.work_item_id = o.work_item_id
                 WHERE w.work_item_id = ?1 AND w.agent_id = ?2 AND w.state = 'completed'
                   AND json_extract(w.payload_json, '$.result_brief_id') = ?3",
                params![work_id, brief.agent_id, brief.id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(principal) = principal {
            record_cause(
                conn,
                brief,
                &principal,
                NotificationKind::WorkItemCompleted,
                work_id,
                Some(work_id),
            )?;
        }
    }
    for wait_id in wait_ids {
        let Some(turn_id) = brief.turn_id.as_deref() else {
            continue;
        };
        if let Some((principal, work_id)) = wait_recipient(conn, wait_id, &brief.agent_id, turn_id)?
        {
            record_cause(
                conn,
                brief,
                &principal,
                NotificationKind::OperatorInputRequired,
                wait_id,
                work_id.as_deref(),
            )?;
        }
    }
    Ok(())
}

fn record_cause(
    conn: &Connection,
    brief: &BriefRecord,
    principal: &str,
    kind: NotificationKind,
    cause_id: &str,
    work_id: Option<&str>,
) -> Result<()> {
    // The FK is immediate, so create the display entry before binding the
    // cause, but only when the cause is still unclaimed.
    let priority = kind.priority();
    let stored = conn.execute(
        "INSERT INTO operator_inbox_recent
             (principal_id, brief_id, agent_id, work_item_id, priority, created_at)
         SELECT ?1, ?2, ?3, ?4, ?5, ?6
         WHERE NOT EXISTS (
             SELECT 1 FROM operator_inbox_causes
             WHERE principal_id = ?1 AND priority = ?5 AND cause_id = ?7
         )
         ON CONFLICT(principal_id, brief_id) DO UPDATE SET
             priority = max(priority, excluded.priority),
             work_item_id = CASE WHEN excluded.priority > priority
                                 THEN excluded.work_item_id ELSE work_item_id END",
        params![
            principal,
            brief.id,
            brief.agent_id,
            work_id,
            priority,
            brief.created_at.to_rfc3339_opts(SecondsFormat::Nanos, true),
            cause_id
        ],
    )?;
    if stored > 0 {
        conn.execute(
            "INSERT INTO operator_inbox_causes (principal_id, priority, cause_id, brief_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![principal, priority, cause_id, brief.id],
        )?;
    }
    Ok(())
}

pub(crate) fn recent(
    conn: &Connection,
    recipient: &NotificationRecipient,
    now: DateTime<Utc>,
) -> Result<Vec<InboxNotification>> {
    let cutoff = (now - chrono::Duration::days(30)).to_rfc3339_opts(SecondsFormat::Nanos, true);
    let mut stmt = conn.prepare_cached(
        "SELECT r.brief_id, r.priority, r.created_at, r.agent_id, w.work_item_id
         FROM operator_inbox_recent r
         JOIN agent_identities i ON i.agent_id = r.agent_id AND i.status = 'active'
         LEFT JOIN work_items w ON w.work_item_id = r.work_item_id AND w.agent_id = r.agent_id
         WHERE r.principal_id = ?1 AND r.created_at >= ?2
           AND (i.visibility = 'public' OR ?3)
         ORDER BY r.created_at DESC, r.brief_id LIMIT 100",
    )?;
    let mut rows = stmt.query(params![
        recipient.principal_id,
        cutoff,
        recipient.local_control
    ])?;
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        let kind = match row.get::<_, i64>(1)? {
            1 => NotificationKind::OperatorFirstBrief,
            2 => NotificationKind::WorkItemCompleted,
            3 => NotificationKind::OperatorInputRequired,
            _ => anyhow::bail!("invalid operator inbox priority"),
        };
        items.push(InboxNotification {
            id: row.get(0)?,
            kind,
            created_at: row.get::<_, String>(2)?.parse()?,
            target: NotificationTarget {
                agent_id: row.get(3)?,
                work_item_id: row.get(4)?,
            },
        });
    }
    Ok(items)
}

impl NotificationKind {
    fn priority(self) -> i64 {
        match self {
            Self::OperatorFirstBrief => 1,
            Self::WorkItemCompleted => 2,
            Self::OperatorInputRequired => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, JsonSchema, Serialize)]
pub(crate) struct NotificationTarget {
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, JsonSchema, Serialize)]
pub(crate) struct InboxNotification {
    pub id: String,
    pub kind: NotificationKind,
    pub created_at: DateTime<Utc>,
    pub target: NotificationTarget,
}

#[derive(Serialize, JsonSchema)]
pub struct InboxNotificationList {
    pub(crate) items: Vec<InboxNotification>,
}

/// Return the persisted principal only for the runtime's own trusted
/// interaction classes. Do not use actor-shaped data on arbitrary inputs.
fn trusted_operator_principal(message: &MessageEnvelope) -> Option<&str> {
    message.trusted_interaction_id()?;
    match &message.origin {
        MessageOrigin::Operator {
            actor_id: Some(actor_id),
            ..
        } if !actor_id.is_empty() => Some(actor_id),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NotificationRecipient {
    pub principal_id: String,
    pub local_control: bool,
}

impl NotificationRecipient {
    fn can_handle_operator_wait(&self, assigned_principal: Option<&str>) -> bool {
        // An unassigned background wait is actionable in the shared local
        // control plane, but is not implicitly assigned to every OIDC user.
        assigned_principal.map_or(self.local_control, |principal| {
            self.principal_id == principal
        })
    }
}

fn wait_recipient(
    conn: &Connection,
    wait_id: &str,
    agent_id: &str,
    turn_id: &str,
) -> Result<Option<(String, Option<String>)>> {
    let row = conn
        .query_row(
            "SELECT w.work_item_id, w.wake_sources_json, m.payload_json, o.principal_id, EXISTS (
             SELECT 1 FROM json_each(t.payload_json, '$.input_message_ids') inputs
             JOIN messages other ON other.evidence_id = inputs.value AND other.agent_id = w.agent_id
             WHERE other.kind = 'operator_prompt' AND other.evidence_id IS NOT t.trigger_message_id
         )
         FROM wait_conditions w
         JOIN agent_identities i ON i.agent_id = w.agent_id AND i.status = 'active'
         JOIN turn_records t ON t.turn_id = w.last_turn_id AND t.agent_id = w.agent_id
         LEFT JOIN messages m ON m.evidence_id = t.trigger_message_id AND m.agent_id = w.agent_id
         LEFT JOIN operator_inbox_work_owners o ON o.work_item_id = w.work_item_id
         WHERE w.wait_condition_id = ?1 AND w.agent_id = ?2 AND w.last_turn_id = ?3
           AND w.source = 'WaitFor' AND w.kind = 'operator' AND w.status = 'active'
           AND (w.work_item_id IS NULL OR EXISTS (
               SELECT 1 FROM work_items k WHERE k.work_item_id = w.work_item_id
                 AND k.agent_id = w.agent_id AND k.state = 'open'
           ))",
            params![wait_id, agent_id, turn_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, bool>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((work_id, wake_json, input, owner, ambiguous)) = row else {
        return Ok(None);
    };
    if !serde_json::from_str::<Vec<WakeSource>>(&wake_json)?.contains(&WakeSource::OperatorInput) {
        return Ok(None);
    }
    let input: Option<MessageEnvelope> = input.as_deref().map(serde_json::from_str).transpose()?;
    let principal = input
        .as_ref()
        .and_then(trusted_operator_principal)
        .or(owner.as_deref())
        .unwrap_or("control");
    if ambiguous && principal != "control" {
        return Ok(None);
    }
    Ok(Some((principal.to_owned(), work_id)))
}

/// Pending is current authority: no notification table owns a second copy of
/// waiting state. Check the registered wake, live agent and exact work owner
/// in the same read, and never silently truncate the resulting snapshot.
pub(crate) fn pending(
    conn: &Connection,
    recipient: &NotificationRecipient,
    now: DateTime<Utc>,
) -> Result<Vec<InboxNotification>> {
    let mut stmt = conn.prepare_cached(
        "SELECT w.wait_condition_id, w.agent_id, w.work_item_id, w.status, w.kind, w.source,
                w.subject_ref, w.waiting_for, w.created_at, w.updated_at, w.expires_at,
                w.resolved_at, w.cancelled_at, w.last_turn_id, w.trigger_message_id, w.triggered_at,
                w.wake_sources_json, w.continuation_json, m.payload_json, o.principal_id,
                EXISTS (
                    SELECT 1 FROM json_each(t.payload_json, '$.input_message_ids') inputs
                    JOIN messages other ON other.evidence_id = inputs.value
                     AND other.agent_id = w.agent_id
                    WHERE other.kind = 'operator_prompt'
                      AND other.evidence_id IS NOT t.trigger_message_id
                )
         FROM wait_conditions w
         JOIN agent_identities i ON i.agent_id = w.agent_id AND i.status = 'active'
         LEFT JOIN agent_states a ON a.agent_id = w.agent_id
         LEFT JOIN turn_records t ON t.turn_id = w.last_turn_id AND t.agent_id = w.agent_id
         LEFT JOIN messages m
           ON m.evidence_id = t.trigger_message_id
          AND m.agent_id = w.agent_id
         LEFT JOIN operator_inbox_work_owners o ON o.work_item_id = w.work_item_id
         WHERE w.status = 'active' AND w.kind = 'operator' AND w.source = 'WaitFor'
           AND COALESCE(a.status, '') NOT IN ('stopped', 'paused')
           AND (i.visibility = 'public' OR ?1)
           AND (w.work_item_id IS NULL OR EXISTS (
                   SELECT 1 FROM work_items k
                   WHERE k.work_item_id = w.work_item_id AND k.agent_id = w.agent_id
                     AND k.state = 'open'
               ))",
    )?;
    let mut rows = stmt.query([recipient.local_control])?;
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        let wait = holon_storage::wait::decode_wait_condition_row(row)?;
        if !wait.wake_sources.contains(&WakeSource::OperatorInput)
            || wait.expires_at.is_some_and(|at| at <= now)
        {
            continue;
        }
        let input: Option<String> = row.get(18)?;
        let input: Option<MessageEnvelope> =
            input.as_deref().map(serde_json::from_str).transpose()?;
        let owner: Option<String> = row.get(19)?;
        let has_other_operator_input: bool = row.get(20)?;
        let assigned_principal = input
            .as_ref()
            .and_then(trusted_operator_principal)
            .or(owner.as_deref());
        if (!recipient.local_control && has_other_operator_input)
            || !recipient.can_handle_operator_wait(assigned_principal)
        {
            continue;
        }
        items.push(InboxNotification {
            id: wait.id,
            kind: NotificationKind::OperatorInputRequired,
            created_at: wait.created_at,
            target: NotificationTarget {
                agent_id: wait.agent_id,
                work_item_id: wait.work_item_id,
            },
        });
    }
    items.sort_unstable_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(items)
}

#[cfg(all(test, holon_lib_test_shard = "b"))]
mod tests {
    use super::*;
    use crate::runtime_db::tests::test_support::TempRuntimeDb;
    use crate::types::{
        AdmissionContext, AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset,
        AgentVisibility, AuthorityClass, MessageBody, MessageDeliverySurface, MessageKind,
        Priority, TurnRecord, TurnTriggerSummary, WaitConditionKind, WaitConditionRecord,
        WaitConditionStatus,
    };

    #[test]
    fn first_formal_brief_is_atomic_and_deduplicated_by_input_and_recipient() -> Result<()> {
        use crate::runtime_db::evidence::append_brief_with_created_event_tx;
        use crate::types::{brief_created_event_for, BriefKind};

        let fixture = TempRuntimeDb::new()?;
        let db = &fixture.db;
        let agent = "inbox-agent";
        db.agent_identities().upsert(&AgentIdentityRecord::new(
            agent,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))?;
        let message = |principal: &str| {
            MessageEnvelope::new(
                agent,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: Some(principal.into()),
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Interject,
                MessageBody::Text {
                    text: "question".into(),
                },
            )
            .with_admission(
                MessageDeliverySurface::HttpControlPrompt,
                AdmissionContext::ControlAuthenticated,
            )
        };
        let alice = message("user-alice");
        let bob = message("user-bob");
        db.messages().upsert(&alice)?;
        db.messages().upsert(&bob)?;
        let publish = |brief: &BriefRecord, rollback: bool| -> Result<()> {
            let event = brief_created_event_for(brief)?;
            db.transaction(|tx| {
                append_brief_with_created_event_tx(tx, Some(agent), brief, &event, &[])?;
                anyhow::ensure!(!rollback, "abort publication");
                Ok(())
            })
        };
        let read = |principal: &str| {
            db.with_read(|conn| {
                recent(
                    conn,
                    &NotificationRecipient {
                        principal_id: principal.into(),
                        local_control: false,
                    },
                    Utc::now(),
                )
            })
        };
        let ack = BriefRecord::new(
            agent,
            BriefKind::Ack,
            "queued",
            Some(alice.id.clone()),
            None,
        );
        publish(&ack, false)?;
        let first = BriefRecord::new(
            agent,
            BriefKind::Result,
            "first",
            Some(alice.id.clone()),
            None,
        );
        assert!(publish(&first, true).is_err());
        assert!(read("user-alice")?.is_empty());
        publish(&first, false)?;
        publish(&first, false)?;
        publish(
            &BriefRecord::new(agent, BriefKind::Result, "second", Some(alice.id), None),
            false,
        )?;
        let b = BriefRecord::new(
            agent,
            BriefKind::Result,
            "reply for Bob",
            Some(bob.id),
            None,
        );
        publish(&b, false)?;
        assert_eq!(
            read("user-alice")?
                .iter()
                .map(|item| (&item.id, item.kind))
                .collect::<Vec<_>>(),
            vec![(&first.id, NotificationKind::OperatorFirstBrief)]
        );
        assert_eq!(read("user-bob")?[0].id, b.id);
        assert!(read("user-other")?.is_empty());
        Ok(())
    }

    #[test]
    fn work_completion_requires_its_canonical_brief_and_a_persisted_owner() -> Result<()> {
        use crate::runtime_db::{
            evidence::append_brief_with_created_event_tx,
            transitions::{apply_work_item_mutation_tx, WorkItemMutation},
        };
        use crate::types::{brief_created_event_for, BriefKind, WorkItemRecord, WorkItemState};

        let fixture = TempRuntimeDb::new()?;
        let db = &fixture.db;
        let agent = "inbox-work-agent";
        db.agent_identities().upsert(&AgentIdentityRecord::new(
            agent,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))?;
        let read = |principal: &str| {
            db.with_read(|conn| {
                recent(
                    conn,
                    &NotificationRecipient {
                        principal_id: principal.into(),
                        local_control: false,
                    },
                    Utc::now(),
                )
            })
        };
        for principal in [None, Some("user-alice")] {
            let mut record = WorkItemRecord::new(agent, "make a report", WorkItemState::Open);
            let create = match principal {
                Some(principal) => WorkItemMutation::InsertForOperator {
                    record: record.clone(),
                    principal_id: principal.into(),
                },
                None => WorkItemMutation::Insert {
                    record: record.clone(),
                },
            };
            db.transaction(|tx| {
                assert!(apply_work_item_mutation_tx(tx, &create)?);
                Ok(())
            })?;
            let mut progress =
                BriefRecord::new(agent, BriefKind::Result, "not complete", None, None);
            progress.work_item_id = Some(record.id.clone());
            let progress_event = brief_created_event_for(&progress)?;
            db.transaction(|tx| {
                append_brief_with_created_event_tx(
                    tx,
                    Some(agent),
                    &progress,
                    &progress_event,
                    &[],
                )?;
                Ok(())
            })?;
            assert!(read("user-alice")?.is_empty());
            let mut result = BriefRecord::new(agent, BriefKind::Result, "complete", None, None);
            result.work_item_id = Some(record.id.clone());
            record.state = WorkItemState::Completed;
            record.updated_at = result.created_at;
            record.revision += 1;
            record.result_brief_id = Some(result.id.clone());
            let complete = WorkItemMutation::Update {
                record,
                expected_revision: 1,
            };
            let result_event = brief_created_event_for(&result)?;
            db.transaction(|tx| {
                assert!(apply_work_item_mutation_tx(tx, &complete)?);
                append_brief_with_created_event_tx(tx, Some(agent), &result, &result_event, &[])?;
                Ok(())
            })?;
            let items = read("user-alice")?;
            if principal.is_some() {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].kind, NotificationKind::WorkItemCompleted);
                assert_eq!(items[0].id, result.id);
            } else {
                assert!(items.is_empty());
            }
            assert!(read("user-bob")?.is_empty());
        }
        Ok(())
    }

    #[test]
    fn pending_wait_uses_committed_wake_and_operator_principal() -> Result<()> {
        let fixture = TempRuntimeDb::new()?;
        let db = &fixture.db;
        let agent = "inbox-agent";
        let now = Utc::now();
        db.agent_identities().upsert(&AgentIdentityRecord::new(
            agent,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))?;
        let message = |actor: &str| {
            MessageEnvelope::new(
                agent,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: Some(actor.to_owned()),
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Interject,
                MessageBody::Text {
                    text: "review after the run".into(),
                },
            )
            .with_admission(
                MessageDeliverySurface::HttpControlPrompt,
                AdmissionContext::ControlAuthenticated,
            )
        };
        let recipient = |principal_id: &str, local_control| NotificationRecipient {
            principal_id: principal_id.to_owned(),
            local_control,
        };
        let alice = message("user-alice");
        db.messages().upsert(&alice)?;
        let mut turn = TurnRecord::new(agent, "inbox-turn", 1);
        turn.trigger = Some(TurnTriggerSummary::from_message(&alice));
        turn.input_message_ids.push(alice.id);
        db.turn_records().upsert(&turn)?;

        let mut wait = WaitConditionRecord {
            id: "inbox-wait".into(),
            agent_id: agent.into(),
            work_item_id: None,
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Operator,
            source: Some("WaitFor".into()),
            subject_ref: None,
            waiting_for: "internal text, not inbox metadata".into(),
            wake_sources: vec![WakeSource::OperatorInput],
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: Some(turn.turn_id.clone()),
            trigger_message_id: None,
            triggered_at: None,
        };
        db.wait_conditions().upsert(&wait)?;

        let alice_recipient = recipient("user-alice", false);
        let bob_recipient = recipient("user-bob", false);
        let local = recipient("control", true);
        let read =
            |recipient: &NotificationRecipient| db.with_read(|conn| pending(conn, recipient, now));
        let items = read(&alice_recipient)?;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, wait.id);
        assert!(read(&bob_recipient)?.is_empty());
        assert!(read(&local)?.is_empty());
        assert_eq!(
            read(&alice_recipient)?,
            items,
            "reading cannot resolve a wait"
        );
        assert!(db
            .with_read(|conn| recent(conn, &alice_recipient, now))?
            .is_empty());

        let mut report = BriefRecord::new(
            agent,
            crate::types::BriefKind::Result,
            "please confirm",
            turn.trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.clone()),
            None,
        );
        report.created_at = now;
        report.turn_id = Some(turn.turn_id.clone());
        db.transaction(|tx| {
            use crate::runtime_db::evidence::{
                append_audit_event_ack_tx, insert_brief_evidence_tx,
                link_transition_brief_created_events_tx,
            };
            let registered = crate::types::AuditEvent::legacy(
                "wait_condition_registered",
                serde_json::json!({ "wait_condition_id": wait.id }),
            );
            let created = crate::types::brief_created_event_for(&report)?;
            insert_brief_evidence_tx(tx, &report)?;
            let (registered, _) = append_audit_event_ack_tx(tx, Some(agent), &registered)?;
            let (created, _) = append_audit_event_ack_tx(tx, Some(agent), &created)?;
            link_transition_brief_created_events_tx(
                tx,
                std::slice::from_ref(&report),
                &[registered, created],
            )
        })?;
        let reports = db.with_read(|conn| recent(conn, &alice_recipient, now))?;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].id, report.id);
        assert_eq!(reports[0].kind, NotificationKind::OperatorInputRequired);
        assert!(db
            .with_read(|conn| recent(conn, &bob_recipient, now))?
            .is_empty());

        let bob = message("user-bob");
        db.messages().upsert(&bob)?;
        turn.input_message_ids.push(bob.id);
        db.turn_records().upsert(&turn)?;
        assert!(
            read(&alice_recipient)?.is_empty(),
            "ambiguous operator turn"
        );
        assert!(read(&bob_recipient)?.is_empty());

        // An unassigned background wait is still actionable in local control.
        wait.id = "background-wait".into();
        wait.turn_id = None;
        db.wait_conditions().upsert(&wait)?;
        assert_eq!(read(&local)?[0].id, wait.id);
        wait.status = WaitConditionStatus::Cancelled;
        wait.cancelled_at = Some(now);
        db.wait_conditions().upsert(&wait)?;
        assert!(read(&local)?.is_empty());

        use crate::runtime_db::transitions::{apply_work_item_mutation_tx, WorkItemMutation};
        use crate::types::{WorkItemRecord, WorkItemState};
        let work = WorkItemRecord::new(agent, "operator-owned work", WorkItemState::Open);
        db.transaction(|tx| {
            apply_work_item_mutation_tx(
                tx,
                &WorkItemMutation::InsertForOperator {
                    record: work.clone(),
                    principal_id: "user-alice".into(),
                },
            )?;
            Ok(())
        })?;
        wait.id = "owned-background-wait".into();
        wait.work_item_id = Some(work.id.clone());
        wait.status = WaitConditionStatus::Active;
        wait.cancelled_at = None;
        db.wait_conditions().upsert(&wait)?;
        assert_eq!(
            read(&alice_recipient)?[0].target.work_item_id,
            Some(work.id)
        );
        assert!(read(&bob_recipient)?.is_empty());
        assert!(read(&local)?.is_empty());
        Ok(())
    }
}
