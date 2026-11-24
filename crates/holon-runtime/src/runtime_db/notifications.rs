//! The operator inbox projects committed facts, not agent-authored text.

use anyhow::Result;
use chrono::{DateTime, Utc};
use holon_storage::notifications::{self as storage, record_cause};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Serialize;

use crate::types::{BriefRecord, MessageEnvelope, MessageOrigin};

pub(super) use storage::record_work_owner;
pub(crate) use storage::{InboxNotification, NotificationKind, NotificationTarget};

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
        if let Some(input) = storage::related_input(conn, input_id, &brief.agent_id)? {
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
        let principal = storage::completed_work_owner(conn, work_id, &brief.agent_id, &brief.id)?;
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

#[derive(Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct InboxNotificationList {
    pub(crate) items: Vec<InboxNotification>,
}

impl super::RuntimeDb {
    pub async fn notification_snapshot(
        &self,
        recipient: NotificationRecipient,
    ) -> Result<(InboxNotificationList, InboxNotificationList)> {
        self.with_read_async(move |conn| {
            let now = Utc::now();
            let view = conn.transaction()?;
            let pending = InboxNotificationList {
                items: pending(&view, &recipient, now)?,
            };
            let recent = InboxNotificationList {
                items: recent(&view, &recipient, now)?,
            };
            view.commit()?;
            Ok((pending, recent))
        })
        .await
    }

    pub async fn pending_notifications(
        &self,
        recipient: NotificationRecipient,
    ) -> Result<InboxNotificationList> {
        self.with_read_async(move |conn| {
            Ok(InboxNotificationList {
                items: pending(conn, &recipient, Utc::now())?,
            })
        })
        .await
    }

    pub async fn recent_notifications(
        &self,
        recipient: NotificationRecipient,
    ) -> Result<InboxNotificationList> {
        self.with_read_async(move |conn| {
            Ok(InboxNotificationList {
                items: recent(conn, &recipient, Utc::now())?,
            })
        })
        .await
    }
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
pub struct NotificationRecipient {
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
    let Some(storage::WaitOrigin {
        work_item_id,
        input,
        owner,
        ambiguous,
    }) = storage::load_wait_origin(conn, wait_id, agent_id, turn_id)?
    else {
        return Ok(None);
    };
    let principal = input
        .as_ref()
        .and_then(trusted_operator_principal)
        .or(owner.as_deref())
        .unwrap_or("control");
    if ambiguous && principal != "control" {
        return Ok(None);
    }
    Ok(Some((principal.to_owned(), work_item_id)))
}

pub(crate) fn pending(
    conn: &Connection,
    recipient: &NotificationRecipient,
    now: DateTime<Utc>,
) -> Result<Vec<InboxNotification>> {
    storage::pending(
        conn,
        recipient.local_control,
        now,
        |input, owner, has_other_operator_input| {
            let assigned_principal = input.and_then(trusted_operator_principal).or(owner);
            (recipient.local_control || !has_other_operator_input)
                && recipient.can_handle_operator_wait(assigned_principal)
        },
    )
}

pub(crate) fn recent(
    conn: &Connection,
    recipient: &NotificationRecipient,
    now: DateTime<Utc>,
) -> Result<Vec<InboxNotification>> {
    storage::recent(conn, &recipient.principal_id, recipient.local_control, now)
}

#[cfg(all(test, holon_lib_test_shard = "b"))]
mod tests {
    use super::*;
    use crate::runtime_db::tests::test_support::TempRuntimeDb;
    use crate::types::WakeSource;
    use crate::types::{
        AdmissionContext, AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset,
        AgentVisibility, AuthorityClass, MessageBody, MessageDeliverySurface, MessageKind,
        Priority, TurnRecord, TurnTriggerSummary, WaitConditionKind, WaitConditionRecord,
        WaitConditionStatus,
    };
    use rusqlite::params;

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
