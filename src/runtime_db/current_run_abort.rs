//! A current-run cancellation is a durable input fence, not a lifecycle Stop.

use anyhow::{ensure, Context, Result};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use crate::{
    domain::execution_protocol::{ExecutionAttempt, ExecutionAttemptState},
    runtime_db::transitions::AgentStateMutation,
};

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS current_run_abort_intents (
    agent_id TEXT NOT NULL,
    identity_incarnation INTEGER NOT NULL,
    run_id TEXT NOT NULL,
    source_message_id TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    PRIMARY KEY (agent_id, identity_incarnation, run_id)
);
CREATE INDEX IF NOT EXISTS idx_current_run_abort_source
    ON current_run_abort_intents(agent_id, identity_incarnation, source_message_id);
CREATE INDEX IF NOT EXISTS idx_execution_attempt_source_run
    ON execution_protocol_attempts(
        agent_id, json_extract(payload_json,'$.source_message_id'),
        json_extract(payload_json,'$.run_id'));
CREATE INDEX IF NOT EXISTS idx_turn_records_operator_abort_terminal
    ON turn_records(
        agent_id, json_extract(payload_json,'$.terminal.reason'),
        json_extract(payload_json,'$.terminal.kind'));
"#;

pub(super) const HISTORICAL_ABORTED_TURN_SQL: &str = "
SELECT EXISTS(
    SELECT 1 FROM turn_records t
    WHERE t.agent_id=?1
      AND json_extract(t.payload_json,'$.terminal.reason')='operator_aborted'
      AND json_extract(t.payload_json,'$.terminal.kind') IN ('interrupted','aborted')
      AND (json_extract(t.payload_json,'$.trigger.message_id')=?2
           OR EXISTS(SELECT 1 FROM json_each(t.payload_json,'$.input_message_ids')
                     WHERE value=?2)))";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CurrentRunAbortIntent {
    pub agent_id: String,
    pub identity_incarnation: u64,
    pub owner_epoch: u64,
    pub run_id: String,
    pub runtime_lease_id: String,
    pub activation_id: Option<String>,
    pub source_message_id: String,
    pub turn_id: Option<String>,
    pub stop_after_abort: bool,
    pub requested_at: chrono::DateTime<chrono::Utc>,
}

pub(crate) fn settle_attempt(
    attempt: &ExecutionAttempt,
    at: chrono::DateTime<chrono::Utc>,
) -> crate::domain::execution_protocol::ExecutionProtocolCommand {
    use crate::domain::execution_protocol::*;
    let reason = "operator_aborted".to_owned();
    let outcome = match &attempt.binding {
        ExecutionBinding::WorkItem { .. } => {
            ExecutionOutcome::WorkItem(WorkItemOutcome::Aborted { reason })
        }
        ExecutionBinding::Conversation { .. } | ExecutionBinding::AgentLifecycle { .. } => {
            ExecutionOutcome::Conversation(ConversationOutcome::Aborted { reason })
        }
        ExecutionBinding::Command => {
            ExecutionOutcome::Command(CommandResult::Unsupported { reason })
        }
    };
    ExecutionProtocolCommand::Settle(SettleExecution {
        outcome: ExecutionOutcomeRecord {
            outcome_id: format!("outcome:operator_aborted:{}", attempt.attempt_id),
            attempt_id: attempt.attempt_id.clone(),
            outcome,
            created_at: at.to_rfc3339(),
        },
    })
}

pub(crate) fn validate_terminal_tx(
    tx: &Transaction<'_>,
    turn: &crate::types::TurnRecord,
) -> Result<()> {
    let trigger = turn
        .trigger
        .as_ref()
        .and_then(|trigger| trigger.message_id.as_deref());
    let fenced = turn_abort_intent_tx(tx, &turn.agent_id, &turn.turn_id, trigger)?
        || match trigger {
            Some(source) => input_aborted_tx(tx, &turn.agent_id, source)?,
            None => false,
        };
    if fenced {
        ensure!(
            turn.terminal.as_ref().is_some_and(|terminal| terminal.kind
                == crate::types::TurnTerminalKind::Aborted
                && terminal.reason.as_deref() == Some("operator_aborted")),
            "operator-aborted input cannot commit a different terminal"
        );
        let committed: Option<String> = tx
            .query_row(
                "SELECT json_extract(payload_json,'$.terminal') FROM turn_records
             WHERE agent_id=?1 AND turn_id=?2 AND terminal_kind='aborted'
               AND json_extract(payload_json,'$.terminal.reason')='operator_aborted'",
                params![turn.agent_id, turn.turn_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(committed) = committed {
            if serde_json::from_str::<serde_json::Value>(&committed)?
                != serde_json::to_value(&turn.terminal)?
            {
                return Err(super::RuntimeStateTransitionConflict::concurrent_mutation(
                    "turn_terminal",
                    &turn.turn_id,
                )
                .into());
            }
        }
    }
    Ok(())
}

pub(super) fn persist_intent_tx(
    tx: &Transaction<'_>,
    intent: &CurrentRunAbortIntent,
    state: &AgentStateMutation,
) -> Result<()> {
    let expected = state
        .expected
        .as_deref()
        .context("abort requires original State")?;
    ensure!(
        intent.agent_id == expected.id
            && intent.identity_incarnation > 0
            && !intent.runtime_lease_id.is_empty()
            && !intent.source_message_id.is_empty()
            && expected.current_run_id.as_deref() == Some(intent.run_id.as_str())
            && expected.current_turn_id == intent.turn_id
            && state.record.current_run_id.is_none()
            && (state.record.status == crate::types::AgentStatus::Stopped)
                == intent.stop_after_abort,
        "current-run abort identity changed"
    );
    if let Some(activation_id) = &intent.activation_id {
        let payload: String = tx.query_row(
            "SELECT payload_json FROM execution_protocol_attempts
             WHERE agent_id=?1 AND attempt_id=?2",
            params![intent.agent_id, activation_id],
            |row| row.get(0),
        )?;
        let attempt: ExecutionAttempt = serde_json::from_str(&payload)?;
        ensure!(
            attempt.state == ExecutionAttemptState::Open
                // Open admission precedes the physical run; settlement may
                // attach its run id later. State CAS fences the current run.
                && attempt
                    .run_id
                    .as_deref()
                    .is_none_or(|run_id| run_id == intent.run_id)
                && attempt.source_message_id.as_deref() == Some(intent.source_message_id.as_str()),
            "current-run abort attempt changed"
        );
    }
    let queued: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM queue_entries
         WHERE agent_id=?1 AND message_id=?2 AND status='dequeued')",
        params![intent.agent_id, intent.source_message_id],
        |row| row.get(0),
    )?;
    ensure!(queued, "current-run abort source is no longer claimed");
    tx.execute(
        "INSERT INTO current_run_abort_intents
         (agent_id,identity_incarnation,run_id,source_message_id,payload_json)
         VALUES (?1,?2,?3,?4,?5)",
        params![
            intent.agent_id,
            intent.identity_incarnation,
            intent.run_id,
            intent.source_message_id,
            serde_json::to_string(intent)?
        ],
    )?;
    Ok(())
}

/// Includes live-input attempts attached to the original physical run. A new
/// operator submission has a new message identity and is not fenced.
pub(crate) fn input_aborted_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    message_id: &str,
) -> Result<bool> {
    let incarnation: Option<u64> = tx
        .query_row(
            "SELECT CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1
                         ELSE json_extract(payload_json,'$.incarnation') END
             FROM agent_identities WHERE agent_id=?1",
            [agent_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(incarnation) = incarnation else {
        return Ok(false);
    };
    ensure!(incarnation > 0, "invalid agent identity incarnation");
    let fenced = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM current_run_abort_intents i
         WHERE i.agent_id=?1 AND i.identity_incarnation=?2 AND
           (i.source_message_id=?3 OR EXISTS(
             SELECT 1 FROM execution_protocol_attempts a
             WHERE a.agent_id=i.agent_id
               AND json_extract(a.payload_json,'$.source_message_id')=?3
               AND json_extract(a.payload_json,'$.run_id')=i.run_id)))",
        params![agent_id, incarnation, message_id],
        |row| row.get(0),
    )?;
    if fenced {
        return Ok(true);
    }
    // Compatibility is based on the exact attached attempt/Turn, never a
    // matching prompt, timestamp, WorkItem, or an unqualified reason string.
    // The direct Turn match also covers records persisted before this fence
    // existed, where an operator abort settled as plain Interrupted.
    Ok(tx.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM execution_protocol_attempts a JOIN turn_records t
             ON t.agent_id=a.agent_id
               AND t.turn_id=json_extract(a.payload_json,'$.turn_id')
           WHERE a.agent_id=?1
             AND json_extract(a.payload_json,'$.source_message_id')=?2
             AND json_extract(t.payload_json,'$.terminal.reason')='operator_aborted'
             AND json_extract(t.payload_json,'$.terminal.kind') IN ('interrupted','aborted'))",
        params![agent_id, message_id],
        |row| row.get(0),
    )? || tx.query_row(
        HISTORICAL_ABORTED_TURN_SQL,
        params![agent_id, message_id],
        |row| row.get(0),
    )?)
}

/// An unsettled Turn is fenced by its exact abort intent: the intent recorded
/// for this Turn's run, or for one of the inputs this Turn actually consumed.
pub(crate) fn turn_abort_intent_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    turn_id: &str,
    trigger_message_id: Option<&str>,
) -> Result<bool> {
    let by_turn: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM current_run_abort_intents
         WHERE agent_id=?1 AND json_extract(payload_json,'$.turn_id')=?2)",
        params![agent_id, turn_id],
        |row| row.get(0),
    )?;
    if by_turn {
        return Ok(true);
    }
    let Some(message_id) = trigger_message_id else {
        return Ok(false);
    };
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM current_run_abort_intents
         WHERE agent_id=?1 AND source_message_id=?2)",
        params![agent_id, message_id],
        |row| row.get(0),
    )?)
}

impl super::RuntimeDb {
    pub(crate) fn operator_aborted_input(&self, agent_id: &str, message_id: &str) -> Result<bool> {
        self.read_for_preparation(|tx| input_aborted_tx(tx, agent_id, message_id))
    }
}
