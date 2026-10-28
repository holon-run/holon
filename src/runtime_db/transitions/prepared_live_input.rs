//! Frozen live/provider input commands join the original input and Task receipt.
use super::*;
use crate::runtime::resident_task::{TaskFamily, TaskKey};

impl PreparedInputTransition {
    pub(crate) fn live_input_recovery(
        scope: InputEvidenceScope,
        nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: LiveInputRecoveryCommand,
    ) -> Result<Self> {
        validate_recovery(&command, &scope)?;
        Self::prepare(
            scope,
            nonce,
            prepared_at,
            InputTransitionCommand::LiveInputRecovery(command),
        )
    }

    pub(crate) fn live_input_attachment(
        scope: InputEvidenceScope,
        nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: LiveInputAttachmentCommand,
    ) -> Result<Self> {
        validate_attachment(&command, &scope)?;
        Self::prepare(
            scope,
            nonce,
            prepared_at,
            InputTransitionCommand::LiveInputAttachment(command),
        )
    }

    pub(crate) fn provider_result_handoff(
        scope: InputEvidenceScope,
        nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: ProviderResultHandoffCommand,
    ) -> Result<Self> {
        validate_handoff(&command, &scope)?;
        Self::prepare(
            scope,
            nonce,
            prepared_at,
            InputTransitionCommand::ProviderResultHandoff(command),
        )
    }
}

pub(super) fn validate_attachment(
    command: &LiveInputAttachmentCommand,
    scope: &InputEvidenceScope,
) -> Result<()> {
    validate_pair(&command.expected_attempt, &command.expected_turn, scope)?;
    anyhow::ensure!(
        command.agent_state.record.id == scope.agent_id
            && command
                .agent_state
                .expected
                .as_ref()
                .is_some_and(|state| state.id == scope.agent_id)
            && command
                .messages
                .iter()
                .all(|message| message.agent_id == scope.agent_id)
            && command
                .queues
                .iter()
                .all(|queue| queue.agent_id == scope.agent_id),
        "live input frozen namespace differs"
    );
    Ok(())
}

pub(super) fn validate_handoff(
    command: &ProviderResultHandoffCommand,
    scope: &InputEvidenceScope,
) -> Result<()> {
    validate_pair(&command.expected_attempt, &command.expected_turn, scope)
}

pub(super) fn validate_recovery(
    command: &LiveInputRecoveryCommand,
    scope: &InputEvidenceScope,
) -> Result<()> {
    validate_pair(&command.expected_attempt, &command.expected_turn, scope)?;
    anyhow::ensure!(
        command.predecessor.agent_id == scope.agent_id && !command.predecessor.turn_id.is_empty(),
        "live input recovery frozen predecessor namespace differs"
    );
    Ok(())
}

fn validate_pair(
    attempt: &crate::domain::execution_protocol::ExecutionAttempt,
    turn: &TurnRecord,
    scope: &InputEvidenceScope,
) -> Result<()> {
    anyhow::ensure!(
        attempt.agent_id == scope.agent_id
            && turn.agent_id == scope.agent_id
            && !attempt.attempt_id.is_empty()
            && !turn.turn_id.is_empty(),
        "live input frozen attempt/Turn namespace differs"
    );
    Ok(())
}

const LIVE_RESULTS: &str = "SELECT result_identity, task_id FROM task_result_settlements
     WHERE agent_id=?1 AND result_identity IN (SELECT value FROM json_each(?2))";

const RECOVERY_RESULTS: &str = "SELECT result_identity, task_id FROM task_result_settlements
     WHERE agent_id=?1 AND state='caller_admitted' AND activation_id=?2";

// Addresses only: the original provider helper still validates every source,
// interrupted edge, binding and Turn before any write. A missing proof rejects
// the whole transaction; this selector cannot broaden that business admission.
const PROVIDER_RESULTS: &str = "WITH RECURSIVE predecessors(attempt_id) AS (
       SELECT ?2
       UNION
       SELECT json_extract(attempt.payload_json, '$.recovery_of_attempt_id')
       FROM execution_protocol_attempts attempt
       JOIN predecessors prior ON attempt.attempt_id=prior.attempt_id
       WHERE attempt.agent_id=?1
         AND json_extract(attempt.payload_json, '$.recovery_of_attempt_id') IS NOT NULL
       LIMIT 129
     )
     SELECT result_identity, task_id FROM task_result_settlements
     WHERE agent_id=?1 AND state='caller_admitted' AND activation_id<>?2
       AND activation_id IN (SELECT attempt_id FROM predecessors)";

fn selector(command: &InputTransitionCommand) -> Result<Option<(&'static str, String)>> {
    Ok(match command {
        InputTransitionCommand::LiveInputAttachment(command) => Some((
            LIVE_RESULTS,
            serde_json::to_string(&command.batch.result_identities)?,
        )),
        InputTransitionCommand::LiveInputRecovery(command) => command
            .expected_attempt
            .recovery_of_attempt_id
            .as_ref()
            .map(|id| (RECOVERY_RESULTS, id.clone())),
        InputTransitionCommand::ProviderResultHandoff(command) => Some((
            PROVIDER_RESULTS,
            command.expected_attempt.attempt_id.clone(),
        )),
        _ => None,
    })
}

/// Reserve address copies before constructing the original Task candidate set.
pub(super) fn result_address_material_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
) -> Result<(usize, usize)> {
    let Some((sql, selector)) = selector(prepared.command.as_ref())? else {
        return Ok((0, 0));
    };
    let (bytes, records): (usize, usize) = tx.query_row(
        &format!(
            "SELECT COALESCE(SUM(length(CAST(result_identity AS BLOB))
            +length(CAST(task_id AS BLOB))+256),0), COUNT(*) FROM ({sql})"
        ),
        params![prepared.primary_scope.agent_id, selector],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((
        bytes,
        records
            .checked_mul(2)
            .context("live Result key count overflow")?,
    ))
}

pub(super) fn result_keys_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
) -> Result<BTreeSet<TaskKey>> {
    let Some((sql, selector)) = selector(prepared.command.as_ref())? else {
        return Ok(BTreeSet::new());
    };
    let mut query = tx.prepare(sql)?;
    let mut keys = BTreeSet::new();
    for row in query.query_map(params![prepared.primary_scope.agent_id, selector], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (result, task) = row?;
        keys.insert(TaskKey::new(TaskFamily::ResultSettlement, result));
        keys.insert(TaskKey::new(TaskFamily::Task, task));
    }
    Ok(keys)
}
