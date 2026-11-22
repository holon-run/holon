//! Withdrawal of a single unexecuted operator input.
//!
//! Withdrawal competes atomically with queue claim/admission through the
//! writer transaction: it succeeds only by transitioning the queue entry from
//! `queued` to the terminal `withdrawn` state, and claim succeeds only from
//! `queued`. The terminal marker is metadata-only and makes client retries
//! idempotent after a network timeout. See
//! docs/rfcs/conversation-turn-deletion.md §7.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Transaction};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use holon_storage::codec::timestamp;
use holon_storage::index_outbox::{
    insert_runtime_index_changes_tx, RuntimeIndexChange, RuntimeIndexOperation,
};

use crate::runtime_db::RuntimeDb;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputWithdrawalError {
    /// No such message id for this agent.
    NotFound,
    /// The input is not operator-submitted and stays out of scope.
    NotOperatorInput,
    /// Execution already claimed or processed the input; stop the execution
    /// and delete the whole turn after settlement instead.
    AlreadyClaimed { status: String },
}

impl std::fmt::Display for InputWithdrawalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "message not found for this agent"),
            Self::NotOperatorInput => write!(
                f,
                "only operator-submitted unexecuted input can be withdrawn"
            ),
            Self::AlreadyClaimed { status } => write!(
                f,
                "input already claimed for execution (status: {status}); stop the execution \
                 and delete the turn after settlement"
            ),
        }
    }
}

impl std::error::Error for InputWithdrawalError {}

/// Successful withdrawal outcome. `first_withdrawal` is false for idempotent
/// retries that found the terminal marker.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct InputWithdrawal {
    pub agent_id: String,
    pub message_id: String,
    pub withdrawn_at: String,
    pub first_withdrawal: bool,
}

pub fn withdraw_input(
    db: &RuntimeDb,
    agent_id: &str,
    message_id: &str,
    now: DateTime<Utc>,
) -> Result<Result<InputWithdrawal, InputWithdrawalError>> {
    db.transaction(|tx| {
        let entry = load_queue_entry(tx, agent_id, message_id)?;
        let Some(entry) = entry else {
            return Ok(Err(InputWithdrawalError::NotFound));
        };
        if entry.status == "withdrawn" {
            // Idempotent retry: the message row is already gone and the
            // queue entry keeps only the metadata marker.
            return Ok(Ok(InputWithdrawal {
                agent_id: agent_id.to_owned(),
                message_id: message_id.to_owned(),
                withdrawn_at: entry.updated_at,
                first_withdrawal: false,
            }));
        }
        if entry.status != "queued" {
            return Ok(Err(InputWithdrawalError::AlreadyClaimed {
                status: entry.status,
            }));
        }
        // An assigned input was already claimed by a turn even if the queue
        // status lags; refuse rather than racing the claim.
        let assigned: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM conversation_input_assignments
              WHERE agent_id = ?1 AND message_id = ?2)",
            params![agent_id, message_id],
            |row| row.get(0),
        )?;
        if assigned {
            return Ok(Err(InputWithdrawalError::AlreadyClaimed {
                status: "assigned".to_owned(),
            }));
        }
        let origin_kind = tx
            .query_row(
                "SELECT json_extract(payload_json, '$.origin.kind') FROM messages
                  WHERE evidence_id = ?1 AND agent_id = ?2",
                params![message_id, agent_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        if origin_kind.as_deref() != Some("operator") {
            return Ok(Err(InputWithdrawalError::NotOperatorInput));
        }

        let withdrawn_at = timestamp(now);
        let updated_payload = withdrawn_payload_json(&entry.payload_json, &withdrawn_at)?;
        // Atomic CAS against claim: only a still-queued entry can transition.
        let updated = tx.execute(
            "UPDATE queue_entries
                SET status = 'withdrawn', updated_at = ?3, payload_json = ?4
              WHERE agent_id = ?1 AND message_id = ?2 AND status = 'queued'",
            params![agent_id, message_id, withdrawn_at, updated_payload],
        )?;
        if updated != 1 {
            return Ok(Err(InputWithdrawalError::AlreadyClaimed {
                status: "claim_race".to_owned(),
            }));
        }
        tx.execute(
            "DELETE FROM messages WHERE evidence_id = ?1 AND agent_id = ?2",
            params![message_id, agent_id],
        )?;
        insert_runtime_index_changes_tx(
            tx,
            &[RuntimeIndexChange {
                agent_id: agent_id.to_owned(),
                source_kind: "message".to_owned(),
                source_id: message_id.to_owned(),
                source_ref: format!("message:{message_id}"),
                operation: RuntimeIndexOperation::Delete,
                source_updated_at: Some(now),
                reason: "input_withdrawn".to_owned(),
            }],
        )?;
        append_withdrawal_audit_event(tx, agent_id, message_id, &withdrawn_at)?;
        Ok(Ok(InputWithdrawal {
            agent_id: agent_id.to_owned(),
            message_id: message_id.to_owned(),
            withdrawn_at,
            first_withdrawal: true,
        }))
    })
}

struct QueueEntry {
    status: String,
    updated_at: String,
    payload_json: String,
}

fn load_queue_entry(
    tx: &Transaction<'_>,
    agent_id: &str,
    message_id: &str,
) -> Result<Option<QueueEntry>> {
    let row = tx
        .query_row(
            "SELECT status, updated_at, payload_json FROM queue_entries
              WHERE agent_id = ?1 AND message_id = ?2",
            params![agent_id, message_id],
            |row| {
                Ok(QueueEntry {
                    status: row.get(0)?,
                    updated_at: row.get(1)?,
                    payload_json: row.get(2)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

fn withdrawn_payload_json(payload_json: &str, withdrawn_at: &str) -> Result<String> {
    // Keep the metadata-only record parseable for readers that decode
    // QueueEntryRecord; the body lives in `messages`, which is removed.
    let mut payload: Value = serde_json::from_str(payload_json)?;
    if let Some(record) = payload.as_object_mut() {
        record.insert("status".into(), Value::String("withdrawn".into()));
        record.insert("updated_at".into(), Value::String(withdrawn_at.into()));
    }
    Ok(serde_json::to_string(&payload)?)
}

fn append_withdrawal_audit_event(
    tx: &Transaction<'_>,
    agent_id: &str,
    message_id: &str,
    withdrawn_at: &str,
) -> Result<()> {
    let event = holon_domain::AuditEvent::legacy(
        "input_withdrawn",
        serde_json::json!({
            "agent_id": agent_id,
            "message_id": message_id,
            "withdrawn_at": withdrawn_at,
        }),
    );
    // Canonical append path: the allocated `event_seq` carries the
    // `message_id` into every client's change stream as an OperatorRemove.
    super::evidence::append_audit_event_tx(tx, Some(agent_id), &event)?;
    Ok(())
}
