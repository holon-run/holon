//! Durable internal Agent message delivery ledger.

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, Transaction};

use crate::{
    runtime_db::agent_relations::canonical_relations_from_connection,
    runtime_db::AgentMessageDeliveryRepository,
    types::{
        AgentIdentityLifecycle, AgentIdentityRecord, AgentMessageAdmissionEvidence,
        AgentMessageDeliveryError, AgentMessageDeliveryOutcome, AgentMessageDeliveryReceipt,
        AgentMessageDeliveryRecord, AgentMessageDeliveryRejectionCode, AgentMessageDeliveryState,
        AgentPolicyEffect, AgentRegistryStatus, AgentState, AgentStatus,
    },
};

const DELIVERY_DIAGNOSTIC_LIMIT: usize = 512;

pub(crate) enum AgentMessageDeliveryAdmissionDecision {
    Queue {
        record: AgentMessageDeliveryRecord,
        replace_existing: bool,
    },
    Complete {
        record: AgentMessageDeliveryRecord,
        insert: bool,
        replay: bool,
    },
}

impl AgentMessageDeliveryAdmissionDecision {
    pub(crate) fn receipt(&self) -> AgentMessageDeliveryReceipt {
        match self {
            Self::Queue { record, .. } => record.receipt(false),
            Self::Complete { record, replay, .. } => record.receipt(*replay),
        }
    }
}

impl AgentMessageDeliveryRepository<'_> {
    pub fn latest(&self, delivery_id: &str) -> Result<Option<AgentMessageDeliveryRecord>> {
        let connection = self.db.connection()?;
        delivery_by_id(&connection, delivery_id)
    }

    pub fn latest_for_message(
        &self,
        message_id: &str,
    ) -> Result<Option<AgentMessageDeliveryRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT payload_json
                 FROM agent_message_deliveries
                 WHERE message_id = ?1",
                [message_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_delivery(&payload))
            .transpose()
    }

    pub fn receipt(&self, delivery_id: &str) -> Result<Option<AgentMessageDeliveryReceipt>> {
        Ok(self
            .latest(delivery_id)?
            .map(|record| record.receipt(false)))
    }

    pub(crate) fn admit_without_queue(
        &self,
        candidate: &AgentMessageDeliveryRecord,
    ) -> Result<AgentMessageDeliveryReceipt> {
        self.db.transaction(|tx| {
            let decision = prepare_delivery_admission_tx(tx, candidate)?;
            match &decision {
                AgentMessageDeliveryAdmissionDecision::Complete { .. } => {
                    persist_completed_admission_tx(tx, &decision)?;
                    Ok(decision.receipt())
                }
                AgentMessageDeliveryAdmissionDecision::Queue { .. } => {
                    anyhow::bail!("active delivery admission requires a target queue transaction")
                }
            }
        })
    }
}

pub(crate) fn delivery_by_id(
    connection: &rusqlite::Connection,
    delivery_id: &str,
) -> Result<Option<AgentMessageDeliveryRecord>> {
    connection
        .query_row(
            "SELECT payload_json
             FROM agent_message_deliveries
             WHERE delivery_id = ?1",
            [delivery_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| decode_delivery(&payload))
        .transpose()
}

pub(crate) fn delivery_by_idempotency_scope_tx(
    tx: &Transaction<'_>,
    idempotency_scope: &str,
    idempotency_key_digest: &str,
) -> Result<Option<AgentMessageDeliveryRecord>> {
    tx.query_row(
        "SELECT payload_json
         FROM agent_message_deliveries
         WHERE idempotency_scope = ?1 AND idempotency_key_digest = ?2",
        params![idempotency_scope, idempotency_key_digest],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|payload| decode_delivery(&payload))
    .transpose()
}

pub(crate) fn delivery_by_message_id_tx(
    tx: &Transaction<'_>,
    message_id: &str,
) -> Result<Option<AgentMessageDeliveryRecord>> {
    tx.query_row(
        "SELECT payload_json
         FROM agent_message_deliveries
         WHERE message_id = ?1",
        [message_id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|payload| decode_delivery(&payload))
    .transpose()
}

pub(crate) fn prepare_delivery_admission_tx(
    tx: &Transaction<'_>,
    candidate: &AgentMessageDeliveryRecord,
) -> Result<AgentMessageDeliveryAdmissionDecision> {
    let existing = delivery_by_idempotency_scope_tx(
        tx,
        &candidate.idempotency_scope,
        &candidate.idempotency_key_digest,
    )?;
    if let Some(existing) = existing.as_ref() {
        if existing.request_digest != candidate.request_digest {
            return Err(AgentMessageDeliveryError::IdempotencyConflict {
                idempotency_scope: candidate.idempotency_scope.clone(),
                idempotency_key_digest: candidate.idempotency_key_digest.clone(),
            }
            .into());
        }
        if existing.outcome == AgentMessageDeliveryOutcome::Accepted || !existing.retryable {
            return Ok(AgentMessageDeliveryAdmissionDecision::Complete {
                record: existing.clone(),
                insert: false,
                replay: true,
            });
        }
    }

    let identity = tx
        .query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
            [&candidate.target_agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| {
            serde_json::from_str::<AgentIdentityRecord>(&payload)
                .context("decoding delivery target identity")
        })
        .transpose()?;
    let state = tx
        .query_row(
            "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
            [&candidate.target_agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| {
            serde_json::from_str::<AgentState>(&payload)
                .context("decoding delivery target agent state")
        })
        .transpose()?;
    let relations = canonical_relations_from_connection(tx, &candidate.target_agent_id)?;
    let policy = relations
        .as_ref()
        .and_then(|projection| projection.message_policy.as_ref());
    let principal_id = match candidate.caller.principal_kind {
        crate::types::AgentMessagePrincipalKind::SupervisingParent
        | crate::types::AgentMessagePrincipalKind::PeerAgent => candidate
            .caller
            .caller_agent_id
            .as_deref()
            .unwrap_or(&candidate.caller.caller_principal),
        crate::types::AgentMessagePrincipalKind::Operator
        | crate::types::AgentMessagePrincipalKind::ExternalIngress
        | crate::types::AgentMessagePrincipalKind::RuntimeCapability => {
            &candidate.caller.caller_principal
        }
    };
    let matched_rule = policy.and_then(|policy| {
        policy.rules.iter().enumerate().find(|(_, rule)| {
            rule.principal_kind == candidate.caller.principal_kind
                && rule
                    .principal_id
                    .as_deref()
                    .is_none_or(|expected| expected == principal_id)
                && rule
                    .route
                    .as_deref()
                    .is_none_or(|expected| expected == candidate.caller.route)
        })
    });
    let allowed = matched_rule.map_or_else(
        || policy.is_some_and(|policy| policy.default_effect == AgentPolicyEffect::Allow),
        |(_, rule)| rule.effect == AgentPolicyEffect::Allow,
    );
    let evidence = AgentMessageAdmissionEvidence {
        identity_revision: identity.as_ref().map(|identity| identity.revision),
        identity_status: identity.as_ref().map(|identity| match identity.status {
            AgentRegistryStatus::Active => AgentIdentityLifecycle::Active,
            AgentRegistryStatus::Deleting => AgentIdentityLifecycle::Deleting,
            AgentRegistryStatus::Deleted => AgentIdentityLifecycle::Deleted,
        }),
        runtime_status: state
            .as_ref()
            .map(|state| enum_string(&state.status))
            .transpose()?,
        message_policy_revision: policy.map(|policy| policy.revision),
        principal_kind: candidate.caller.principal_kind,
        principal_id: Some(principal_id.to_string()),
        route: candidate.caller.route.clone(),
        matched_rule_index: matched_rule.map(|(index, _)| index),
    };
    let rejection = match identity.as_ref().map(|identity| identity.status) {
        None => Some((
            AgentMessageDeliveryRejectionCode::TargetNotFound,
            "target agent identity was not found",
        )),
        Some(AgentRegistryStatus::Deleting) => Some((
            AgentMessageDeliveryRejectionCode::AgentDeleting,
            "target agent is deletion-fenced",
        )),
        Some(AgentRegistryStatus::Deleted) => Some((
            AgentMessageDeliveryRejectionCode::AgentDeleted,
            "target agent is deleted",
        )),
        Some(AgentRegistryStatus::Active)
            if state
                .as_ref()
                .is_some_and(|state| state.status == AgentStatus::Stopped) =>
        {
            Some((
                AgentMessageDeliveryRejectionCode::AgentStopped,
                "target agent is stopped",
            ))
        }
        Some(AgentRegistryStatus::Active) if !allowed => Some((
            AgentMessageDeliveryRejectionCode::MessageNotAuthorized,
            "caller is not authorized by the target message policy",
        )),
        Some(AgentRegistryStatus::Active) => None,
    };
    let now = Utc::now();
    let replace_existing = existing.is_some();
    let mut record = existing.unwrap_or_else(|| candidate.clone());
    record.admission_evidence = evidence;
    record.updated_at = now;
    record.state_version = record
        .state_version
        .saturating_add(u64::from(replace_existing));
    if let Some((code, diagnostic)) = rejection {
        record.message_id = None;
        record.outcome = AgentMessageDeliveryOutcome::Rejected;
        record.state = AgentMessageDeliveryState::Rejected;
        record.rejection_code = Some(code);
        record.retryable = code.retryable();
        record.diagnostic = Some(bounded_diagnostic(diagnostic));
        record.accepted_at = None;
        record.terminal_at = Some(now);
        return Ok(AgentMessageDeliveryAdmissionDecision::Complete {
            record,
            insert: !replace_existing,
            replay: replace_existing,
        });
    }

    record.message_id.clone_from(&candidate.message_id);
    record.correlation_id.clone_from(&candidate.correlation_id);
    record.causation_id.clone_from(&candidate.causation_id);
    record.outcome = AgentMessageDeliveryOutcome::Accepted;
    record.state = AgentMessageDeliveryState::Queued;
    record.rejection_code = None;
    record.retryable = false;
    record.diagnostic = None;
    record.accepted_at = Some(now);
    record.terminal_at = None;
    Ok(AgentMessageDeliveryAdmissionDecision::Queue {
        record,
        replace_existing,
    })
}

pub(crate) fn insert_delivery_tx(
    tx: &Transaction<'_>,
    record: &AgentMessageDeliveryRecord,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    tx.execute(
        "INSERT INTO agent_message_deliveries (
           delivery_id, target_agent_id, message_id, idempotency_scope,
           idempotency_key_digest, request_digest, outcome, state, state_version,
           rejection_code, retryable, accepted_at, terminal_at, created_at,
           updated_at, payload_json
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16
         )",
        params![
            record.delivery_id,
            record.target_agent_id,
            record.message_id,
            record.idempotency_scope,
            record.idempotency_key_digest,
            record.request_digest,
            enum_string(&record.outcome)?,
            enum_string(&record.state)?,
            i64::try_from(record.state_version).context("delivery state version exceeds SQLite")?,
            record
                .rejection_code
                .as_ref()
                .map(enum_string)
                .transpose()?,
            i64::from(record.retryable),
            record.accepted_at.map(timestamp),
            record.terminal_at.map(timestamp),
            timestamp(record.created_at),
            timestamp(record.updated_at),
            payload_json,
        ],
    )?;
    Ok(())
}

pub(crate) fn persist_completed_admission_tx(
    tx: &Transaction<'_>,
    decision: &AgentMessageDeliveryAdmissionDecision,
) -> Result<()> {
    let AgentMessageDeliveryAdmissionDecision::Complete { record, insert, .. } = decision else {
        return Ok(());
    };
    if *insert {
        insert_delivery_tx(tx, record)
    } else {
        replace_delivery_tx(tx, record)
    }
}

pub(crate) fn persist_queued_admission_tx(
    tx: &Transaction<'_>,
    decision: &AgentMessageDeliveryAdmissionDecision,
) -> Result<()> {
    let AgentMessageDeliveryAdmissionDecision::Queue {
        record,
        replace_existing,
    } = decision
    else {
        return Ok(());
    };
    if *replace_existing {
        replace_delivery_tx(tx, record)
    } else {
        insert_delivery_tx(tx, record)
    }
}

fn replace_delivery_tx(tx: &Transaction<'_>, record: &AgentMessageDeliveryRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let changed = tx.execute(
        "UPDATE agent_message_deliveries
         SET message_id = ?1, outcome = ?2, state = ?3, state_version = ?4,
             rejection_code = ?5, retryable = ?6, accepted_at = ?7,
             terminal_at = ?8, updated_at = ?9, payload_json = ?10
         WHERE delivery_id = ?11",
        params![
            record.message_id,
            enum_string(&record.outcome)?,
            enum_string(&record.state)?,
            i64::try_from(record.state_version).context("delivery state version exceeds SQLite")?,
            record
                .rejection_code
                .as_ref()
                .map(enum_string)
                .transpose()?,
            i64::from(record.retryable),
            record.accepted_at.map(timestamp),
            record.terminal_at.map(timestamp),
            timestamp(record.updated_at),
            payload_json,
            record.delivery_id,
        ],
    )?;
    anyhow::ensure!(changed == 1, "delivery {} not found", record.delivery_id);
    Ok(())
}

pub(crate) fn compare_and_set_delivery_state_tx(
    tx: &Transaction<'_>,
    delivery_id: &str,
    expected_state: AgentMessageDeliveryState,
    next: &AgentMessageDeliveryRecord,
) -> Result<bool> {
    let payload_json = serde_json::to_string(next)?;
    let changed = tx.execute(
        "UPDATE agent_message_deliveries
         SET state = ?1, state_version = ?2, terminal_at = ?3,
             updated_at = ?4, payload_json = ?5
         WHERE delivery_id = ?6 AND state = ?7 AND state_version = ?8",
        params![
            enum_string(&next.state)?,
            i64::try_from(next.state_version).context("delivery state version exceeds SQLite")?,
            next.terminal_at.map(timestamp),
            timestamp(next.updated_at),
            payload_json,
            delivery_id,
            enum_string(&expected_state)?,
            i64::try_from(next.state_version.saturating_sub(1))
                .context("delivery state version exceeds SQLite")?,
        ],
    )?;
    Ok(changed == 1)
}

pub(crate) fn advance_delivery_state_for_message_tx(
    tx: &Transaction<'_>,
    message_id: &str,
    next_state: AgentMessageDeliveryState,
    diagnostic: Option<&str>,
) -> Result<bool> {
    let Some(mut record) = delivery_by_message_id_tx(tx, message_id)? else {
        return Ok(false);
    };
    let valid = matches!(
        (record.state, next_state),
        (
            AgentMessageDeliveryState::Queued,
            AgentMessageDeliveryState::Dispatched
                | AgentMessageDeliveryState::Consumed
                | AgentMessageDeliveryState::Failed
                | AgentMessageDeliveryState::CancelledByDeletion
        ) | (
            AgentMessageDeliveryState::Dispatched,
            AgentMessageDeliveryState::Consumed
                | AgentMessageDeliveryState::Failed
                | AgentMessageDeliveryState::CancelledByDeletion
        )
    );
    if !valid {
        return Ok(false);
    }

    let expected_state = record.state;
    let now = Utc::now();
    record.state = next_state;
    record.state_version = record.state_version.saturating_add(1);
    record.updated_at = now;
    record.terminal_at = next_state.is_terminal().then_some(now);
    if let Some(diagnostic) = diagnostic {
        record.diagnostic = Some(bounded_diagnostic(diagnostic));
    }
    compare_and_set_delivery_state_tx(tx, &record.delivery_id, expected_state, &record)
}

pub(crate) fn cancel_active_deliveries_for_target_tx(
    tx: &Transaction<'_>,
    target_agent_id: &str,
) -> Result<usize> {
    let queued = enum_string(&AgentMessageDeliveryState::Queued)?;
    let dispatched = enum_string(&AgentMessageDeliveryState::Dispatched)?;
    let records = {
        let mut statement = tx.prepare(
            "SELECT payload_json
             FROM agent_message_deliveries
             WHERE target_agent_id = ?1 AND state IN (?2, ?3)
             ORDER BY created_at ASC, delivery_id ASC",
        )?;
        let rows = statement.query_map(params![target_agent_id, queued, dispatched], |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|payload| decode_delivery(&payload))
            .collect::<Result<Vec<_>>>()?
    };

    let mut cancelled = 0;
    for record in records {
        cancelled += usize::from(advance_delivery_state_for_message_tx(
            tx,
            record
                .message_id
                .as_deref()
                .context("accepted delivery is missing its message id")?,
            AgentMessageDeliveryState::CancelledByDeletion,
            Some("target agent entered the deletion fence"),
        )?);
    }
    Ok(cancelled)
}

fn decode_delivery(payload: &str) -> Result<AgentMessageDeliveryRecord> {
    serde_json::from_str(payload).context("decoding agent message delivery payload")
}

fn enum_string<T: serde::Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_value(value)?
        .as_str()
        .context("delivery enum serialized to a non-string value")?
        .to_string())
}

fn timestamp(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn bounded_diagnostic(value: &str) -> String {
    value.chars().take(DELIVERY_DIAGNOSTIC_LIMIT).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::AgentMessageDeliveryService,
        runtime_db::{
            transitions::{QueueMutation, QueueOperation, QueueTransitionCommand},
            RuntimeDb,
        },
        types::{
            AdmissionContext, AgentKind, AgentMessageCallerContext, AgentMessageDeliveryError,
            AgentMessageDeliveryOutcome, AgentMessageDeliveryRejectionCode,
            AgentMessagePrincipalKind, AgentMessageSendRequest, AgentOwnership, AgentProfilePreset,
            AgentState, AgentStatus, AgentVisibility, AuthorityClass, MessageBody,
            MessageDeliverySurface, MessageOrigin, Priority, QueueEntryRecord, QueueEntryStatus,
        },
    };
    use tempfile::TempDir;

    fn runtime_db() -> Result<(TempDir, RuntimeDb)> {
        let dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("state/runtime.sqlite"),
            dir.path().join("state/runtime.lock"),
        )?;
        Ok((dir, db))
    }

    fn seed_target(db: &RuntimeDb, status: AgentStatus) -> Result<()> {
        db.agent_identities().upsert(&AgentIdentityRecord::new(
            "target-agent",
            AgentKind::Named,
            AgentVisibility::Private,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))?;
        let mut state = AgentState::new("target-agent");
        state.status = status;
        db.agent_states().upsert(&state)
    }

    fn caller() -> AgentMessageCallerContext {
        AgentMessageCallerContext {
            caller_principal: "runtime:agent-invocation".into(),
            caller_agent_id: Some("caller-agent".into()),
            principal_kind: AgentMessagePrincipalKind::RuntimeCapability,
            route: "agent_invocation".into(),
            origin: MessageOrigin::Task {
                task_id: "task-delivery".into(),
            },
            authority_class: AuthorityClass::RuntimeInstruction,
            delivery_surface: MessageDeliverySurface::RuntimeSystem,
            admission_context: AdmissionContext::RuntimeOwned,
            current_turn_id: Some("turn-delivery".into()),
            current_task_id: Some("task-delivery".into()),
            current_work_item_id: Some("work-delivery".into()),
        }
    }

    fn request(key: &str, text: &str) -> AgentMessageSendRequest {
        AgentMessageSendRequest {
            target_agent_id: "target-agent".into(),
            content: MessageBody::Text { text: text.into() },
            client_idempotency_key: key.into(),
            correlation_id: Some("correlation-delivery".into()),
            causation_id: Some("message-parent".into()),
            requested_priority: Some(Priority::Normal),
        }
    }

    fn prepare(key: &str, text: &str) -> Result<crate::runtime::PreparedAgentMessageDelivery> {
        AgentMessageDeliveryService::prepare(request(key, text), caller())
    }

    fn admission_command(
        prepared: &crate::runtime::PreparedAgentMessageDelivery,
    ) -> QueueTransitionCommand {
        let now = Utc::now();
        QueueTransitionCommand {
            agent_id: prepared.message.agent_id.clone(),
            operation: QueueOperation::Admit,
            mutation: QueueMutation::Upsert(QueueEntryRecord {
                message_id: prepared.message.id.clone(),
                agent_id: prepared.message.agent_id.clone(),
                priority: prepared.message.priority.clone(),
                status: QueueEntryStatus::Queued,
                created_at: now,
                updated_at: now,
            }),
            scheduler_claim_work_item: None,
            agent_state: None,
            message_evidence: vec![prepared.message.clone()],
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: Vec::new(),
            notify_scheduler: true,
            fault: None,
            brief_evidence: Vec::new(),
        }
    }

    fn queue_transition(
        db: &RuntimeDb,
        current: &QueueEntryRecord,
        operation: QueueOperation,
        status: QueueEntryStatus,
    ) -> Result<()> {
        let mut next = current.clone();
        next.status = status;
        next.updated_at = Utc::now();
        let mutation = match operation {
            QueueOperation::Claim | QueueOperation::Interject => QueueMutation::Consume(next),
            QueueOperation::Settle | QueueOperation::RepairDrop => QueueMutation::Upsert(next),
            QueueOperation::Admit | QueueOperation::Requeue => {
                unreachable!("test helper only advances admitted deliveries")
            }
        };
        db.transitions().commit_queue(&QueueTransitionCommand {
            agent_id: current.agent_id.clone(),
            operation,
            mutation,
            scheduler_claim_work_item: None,
            agent_state: None,
            message_evidence: Vec::new(),
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: Vec::new(),
            notify_scheduler: false,
            fault: None,
            brief_evidence: Vec::new(),
        })?;
        Ok(())
    }

    #[test]
    fn accepted_delivery_is_idempotent_and_survives_restart() -> Result<()> {
        let (dir, db) = runtime_db()?;
        seed_target(&db, AgentStatus::AwakeIdle)?;
        let prepared = prepare("stable-key", "hello")?;
        assert_eq!(prepared.message.turn_id.as_deref(), Some("turn-delivery"));
        let first = db.transitions().commit_delivery_admission(
            &admission_command(&prepared),
            None,
            &prepared.record,
        )?;
        let first = first.delivery_receipt.context("missing delivery receipt")?;
        assert_eq!(first.outcome, AgentMessageDeliveryOutcome::Accepted);
        assert_eq!(first.state, AgentMessageDeliveryState::Queued);
        assert!(!first.idempotent_replay);

        let replay = prepare("stable-key", "hello")?;
        let replay = db.transitions().commit_delivery_admission(
            &admission_command(&replay),
            None,
            &replay.record,
        )?;
        let replay = replay.delivery_receipt.context("missing replay receipt")?;
        assert_eq!(replay.delivery_id, first.delivery_id);
        assert!(replay.idempotent_replay);
        assert_eq!(db.queue_entries().latest_all()?.len(), 1);

        let stored = db
            .agent_message_deliveries()
            .latest(&first.delivery_id)?
            .context("missing delivery ledger record")?;
        assert_eq!(
            stored.admission_evidence.principal_id.as_deref(),
            Some("runtime:agent-invocation")
        );
        assert_eq!(stored.admission_evidence.matched_rule_index, Some(1));
        assert_eq!(
            stored.caller.caller_agent_id.as_deref(),
            Some("caller-agent")
        );

        let database_path = dir.path().join("state/runtime.sqlite");
        let lock_path = dir.path().join("state/runtime.lock");
        drop(db);
        let reopened = RuntimeDb::open_and_migrate(database_path, lock_path)?;
        let receipt = reopened
            .agent_message_deliveries()
            .receipt(&first.delivery_id)?
            .context("delivery receipt did not survive restart")?;
        assert_eq!(receipt.state, AgentMessageDeliveryState::Queued);
        assert_eq!(
            receipt.correlation_id.as_deref(),
            Some("correlation-delivery")
        );
        Ok(())
    }

    #[test]
    fn idempotency_conflict_is_typed_and_does_not_admit_twice() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        seed_target(&db, AgentStatus::AwakeIdle)?;
        let first = prepare("conflict-key", "first")?;
        db.transitions().commit_delivery_admission(
            &admission_command(&first),
            None,
            &first.record,
        )?;

        let conflicting = prepare("conflict-key", "different")?;
        let error = db
            .transitions()
            .commit_delivery_admission(&admission_command(&conflicting), None, &conflicting.record)
            .unwrap_err();
        assert!(error.downcast_ref::<AgentMessageDeliveryError>().is_some());
        assert_eq!(db.queue_entries().latest_all()?.len(), 1);
        Ok(())
    }

    #[test]
    fn retryable_rejection_can_succeed_with_the_same_key() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        seed_target(&db, AgentStatus::Stopped)?;
        let rejected = prepare("retry-key", "retry me")?;
        let rejected = db
            .agent_message_deliveries()
            .admit_without_queue(&rejected.record)?;
        assert_eq!(rejected.outcome, AgentMessageDeliveryOutcome::Rejected);
        assert_eq!(
            rejected.rejection_code,
            Some(AgentMessageDeliveryRejectionCode::AgentStopped)
        );
        assert!(rejected.retryable);

        let mut state = db
            .agent_states()
            .latest("target-agent")?
            .context("missing target state")?;
        state.status = AgentStatus::AwakeIdle;
        db.agent_states().upsert(&state)?;
        let retry = prepare("retry-key", "retry me")?;
        let accepted = db.transitions().commit_delivery_admission(
            &admission_command(&retry),
            None,
            &retry.record,
        )?;
        let accepted = accepted
            .delivery_receipt
            .context("missing accepted retry receipt")?;
        assert_eq!(accepted.delivery_id, rejected.delivery_id);
        assert_eq!(accepted.outcome, AgentMessageDeliveryOutcome::Accepted);
        assert_eq!(accepted.state, AgentMessageDeliveryState::Queued);
        assert!(!accepted.idempotent_replay);
        Ok(())
    }

    #[test]
    fn queue_lifecycle_and_deletion_fence_are_linearized() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        seed_target(&db, AgentStatus::AwakeIdle)?;
        let prepared = prepare("delete-race", "race")?;
        let accepted = db.transitions().commit_delivery_admission(
            &admission_command(&prepared),
            None,
            &prepared.record,
        )?;
        let delivery_id = accepted
            .delivery_receipt
            .context("missing delivery receipt")?
            .delivery_id;
        let queued = db
            .queue_entries()
            .latest(&prepared.message.id)?
            .context("missing queue entry")?;
        queue_transition(
            &db,
            &queued,
            QueueOperation::Claim,
            QueueEntryStatus::Dequeued,
        )?;
        assert_eq!(
            db.agent_message_deliveries()
                .latest(&delivery_id)?
                .context("missing dispatched delivery")?
                .state,
            AgentMessageDeliveryState::Dispatched
        );

        let identity = db
            .agent_identities()
            .latest("target-agent")?
            .context("missing target identity")?;
        db.agent_deletions()
            .begin("target-agent", identity.revision, "test", false)?;
        let cancelled = db
            .agent_message_deliveries()
            .latest(&delivery_id)?
            .context("missing cancelled delivery")?;
        assert_eq!(
            cancelled.state,
            AgentMessageDeliveryState::CancelledByDeletion
        );
        assert!(cancelled.terminal_at.is_some());

        let deleting = prepare("after-fence", "late")?;
        let rejected = db
            .agent_message_deliveries()
            .admit_without_queue(&deleting.record)?;
        assert_eq!(
            rejected.rejection_code,
            Some(AgentMessageDeliveryRejectionCode::AgentDeleting)
        );
        assert!(!rejected.retryable);

        let dequeued = db
            .queue_entries()
            .latest(&prepared.message.id)?
            .context("missing dequeued entry")?;
        queue_transition(
            &db,
            &dequeued,
            QueueOperation::Settle,
            QueueEntryStatus::Processed,
        )?;
        assert_eq!(
            db.agent_message_deliveries()
                .latest(&delivery_id)?
                .context("missing terminal delivery")?
                .state,
            AgentMessageDeliveryState::CancelledByDeletion
        );
        Ok(())
    }

    #[test]
    fn failed_delivery_diagnostic_is_bounded() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        seed_target(&db, AgentStatus::AwakeIdle)?;
        let prepared = prepare("diagnostic-key", "fail")?;
        let accepted = db.transitions().commit_delivery_admission(
            &admission_command(&prepared),
            None,
            &prepared.record,
        )?;
        let delivery_id = accepted
            .delivery_receipt
            .context("missing delivery receipt")?
            .delivery_id;
        let diagnostic = "x".repeat(DELIVERY_DIAGNOSTIC_LIMIT + 100);
        db.transaction(|tx| {
            anyhow::ensure!(advance_delivery_state_for_message_tx(
                tx,
                &prepared.message.id,
                AgentMessageDeliveryState::Failed,
                Some(&diagnostic),
            )?);
            Ok(())
        })?;
        let failed = db
            .agent_message_deliveries()
            .latest(&delivery_id)?
            .context("missing failed delivery")?;
        assert_eq!(failed.state, AgentMessageDeliveryState::Failed);
        assert_eq!(
            failed
                .diagnostic
                .as_deref()
                .context("missing failure diagnostic")?
                .chars()
                .count(),
            DELIVERY_DIAGNOSTIC_LIMIT
        );
        Ok(())
    }
}
