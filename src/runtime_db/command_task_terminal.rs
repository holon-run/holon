//! Original accepted Command terminal results; no new execution admission.
mod recovery;
pub(crate) use recovery::OriginalCommandTaskSparseRecovery;

use crate::runtime::{
    resident_input_evidence::InputEvidenceScope,
    resident_task::{TaskFactLocator, TaskFamily},
};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OriginalCommandTaskAddress {
    pub scope: InputEvidenceScope,
    pub admitted: TaskFactLocator,
    pub owner_epoch: u64,
    pub lease_id: String,
    pub generation_digest: String,
}
impl OriginalCommandTaskAddress {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.scope == self.admitted.scope
                && self.scope.identity_incarnation > 0
                && !self.scope.agent_id.is_empty()
                && self.admitted.key.family == TaskFamily::Task
                && !self.admitted.key.id.is_empty()
                && self.admitted.revision > 0
                && self.owner_epoch > 0
                && !self.lease_id.is_empty()
                && valid_digest(&self.admitted.content_digest)
                && valid_digest(&self.admitted.projection_digest)
                && valid_digest(&self.generation_digest),
            "original Command producer address invalid"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommandTaskTerminalInstallMode {
    Current,
    NoState,
    Retired,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CommandTaskTerminalResult {
    Delivered {
        message_id: String,
    },
    NoOp,
    NotRequested,
    Unavailable {
        reason: CommandTaskTerminalUnavailable,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommandTaskTerminalUnavailable {
    OwnerNoState,
    ScopeRetired,
}
/// The disposition reports this accepted producer, never a desired Task DTO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommandTaskTerminalAck {
    pub original: OriginalCommandTaskAddress,
    pub install_mode: CommandTaskTerminalInstallMode,
    pub task_changed: bool,
    pub result: CommandTaskTerminalResult,
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
impl CommandTaskTerminalAck {
    pub(crate) fn validate(
        &self,
        prepared: &super::input_mutation_receipts::PreparedInputMutation,
    ) -> Result<()> {
        self.original.validate()?;
        ensure!(
            prepared.family() == super::input_mutation_receipts::InputMutationFamily::Task
                && prepared.scopes() == std::slice::from_ref(&self.original.scope),
            "Command terminal ACK differs from original receipt group"
        );
        match (&self.install_mode, &self.result) {
            (
                CommandTaskTerminalInstallMode::Current,
                CommandTaskTerminalResult::Delivered { message_id },
            ) => ensure!(
                !message_id.is_empty(),
                "Command terminal delivery identity empty"
            ),
            (CommandTaskTerminalInstallMode::Current, CommandTaskTerminalResult::NoOp) => {
                ensure!(!self.task_changed, "Command no-op reports a Task write");
            }
            (CommandTaskTerminalInstallMode::Current, CommandTaskTerminalResult::NotRequested) => {}
            (
                CommandTaskTerminalInstallMode::NoState,
                CommandTaskTerminalResult::Unavailable {
                    reason: CommandTaskTerminalUnavailable::OwnerNoState,
                },
            ) => {}
            (
                CommandTaskTerminalInstallMode::Retired,
                CommandTaskTerminalResult::Unavailable {
                    reason: CommandTaskTerminalUnavailable::ScopeRetired,
                },
            ) => ensure!(
                !self.task_changed,
                "retired Command terminal cannot report a Task write"
            ),
            _ => anyhow::bail!("Command terminal disposition differs from installation facts"),
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OriginalCommandResultAddress {
    pub message_id: String,
    pub content_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OriginalCommandTaskBefore {
    pub content_digest: String,
    pub projection_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct OriginalCommandTaskTerminalIntent {
    pub original: OriginalCommandTaskAddress,
    pub before: Option<OriginalCommandTaskBefore>,
    pub mode: CommandTaskTerminalInstallMode,
    pub terminal_digest: String,
    pub requested: Option<OriginalCommandResultAddress>,
    pub no_op: bool,
}

pub(crate) struct CommandTaskOwnerObservation {
    pub mode: CommandTaskTerminalInstallMode,
    pub identity: Option<crate::types::AgentIdentityRecord>,
    _material: std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>,
    extra: Vec<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
}
/// Source provenance is checked by the caller. This reads identity metadata,
/// never a successor State or a Task body, and creates no namespace or alias.
pub(crate) fn observe_original_owner_tx(
    tx: &rusqlite::Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &super::task_snapshot::TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<CommandTaskOwnerObservation> {
    use crate::runtime::input_evidence_budget::{InputMemoryCategory, OwnedBytesEstimate};
    use anyhow::Context;
    use rusqlite::OptionalExtension;
    let n:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)",[&scope.agent_id],|r|r.get(0))?;
    let bytes = n
        .checked_add(scope.agent_id.len() + 256)
        .and_then(|n| n.checked_mul(8))
        .context("Command owner metadata overflow")?;
    ensure!(
        bytes <= request.byte_limit.min(64 * 1024 * 1024) && request.record_limit >= 8,
        super::task_snapshot::TaskSnapshotError::ResourceExhausted(
            "original Command owner metadata"
        )
    );
    let guard = ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(bytes)?,
            metadata_estimate_bytes: 0,
        },
    )?;
    let row: Option<(String, String, String)> = tx
        .query_row(
            "SELECT agent_id,status,payload_json FROM agent_identities WHERE agent_id=?1",
            [&scope.agent_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let identity = match row {
        None => None,
        Some((id, status, raw)) => {
            let identity = super::repositories::decode_agent_identity_payload(&raw)?;
            let incarnation:Option<u64>=tx.query_row("SELECT CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1 WHEN json_type(payload_json,'$.incarnation')='integer' AND json_extract(payload_json,'$.incarnation')>0 THEN json_extract(payload_json,'$.incarnation') END FROM agent_identities WHERE agent_id=?1",[&scope.agent_id],|r|r.get(0))?;
            ensure!(
                id == scope.agent_id
                    && identity.agent_id == id
                    && incarnation == Some(identity.incarnation)
                    && serde_json::to_value(identity.status)?.as_str() == Some(status.as_str()),
                "Command owner identity normalized projection differs"
            );
            Some(identity)
        }
    };
    let current = identity.as_ref().is_some_and(|identity| {
        identity.incarnation == scope.identity_incarnation
            && matches!(
                identity.status,
                crate::types::AgentRegistryStatus::Active
                    | crate::types::AgentRegistryStatus::Deleting
            )
    });
    let mode = if !current {
        CommandTaskTerminalInstallMode::Retired
    } else {
        // Presence is observed only after proving the original incarnation.
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_states WHERE agent_id=?1)",
            [&scope.agent_id],
            |r| r.get(0),
        )?;
        if exists {
            CommandTaskTerminalInstallMode::Current
        } else {
            CommandTaskTerminalInstallMode::NoState
        }
    };
    Ok(CommandTaskOwnerObservation {
        mode,
        identity,
        _material: guard,
        extra: Vec::new(),
    })
}

impl CommandTaskOwnerObservation {
    pub(crate) fn material(
        &self,
    ) -> std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard> {
        self._material.clone()
    }
    pub(crate) fn retain(
        &mut self,
        guard: std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>,
    ) {
        self.extra.push(guard);
    }
    pub(crate) fn into_material(
        self,
    ) -> Vec<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>> {
        let mut guards = self.extra;
        guards.push(self._material);
        guards
    }
}
pub(crate) fn charge_material(
    scope: &InputEvidenceScope,
    bytes: usize,
    records: usize,
    request: &super::task_snapshot::TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>> {
    use crate::runtime::input_evidence_budget::{InputMemoryCategory, OwnedBytesEstimate};
    ensure!(
        bytes <= request.byte_limit.min(64 * 1024 * 1024)
            && records <= request.record_limit.min(65536),
        super::task_snapshot::TaskSnapshotError::ResourceExhausted("original Command material")
    );
    Ok(ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(bytes)?,
            metadata_estimate_bytes: 0,
        },
    )?)
}
pub(crate) fn read_original_state_tx(
    tx: &rusqlite::Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &super::task_snapshot::TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<(
    crate::types::AgentState,
    std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>,
)> {
    use anyhow::Context;
    let bytes:usize=tx.query_row("SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+COALESCE(length(CAST(current_run_id AS BLOB)),0) FROM agent_states WHERE agent_id=?1",[&scope.agent_id],|r|r.get(0))?;
    let guard = charge_material(
        scope,
        bytes
            .checked_add(256)
            .and_then(|n| n.checked_mul(8))
            .context("Command State material overflow")?,
        8,
        request,
        ledger,
    )?;
    let (agent, run, payload): (String, Option<String>, String) = tx.query_row(
        "SELECT agent_id,current_run_id,payload_json FROM agent_states WHERE agent_id=?1",
        [&scope.agent_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let state = super::repositories::decode_agent_state_payload(&payload)?;
    ensure!(
        agent == scope.agent_id && state.id == agent && state.current_run_id == run,
        "Command State normalized projection differs"
    );
    Ok((state, guard))
}
pub(crate) fn merge_original_cancel_flags(
    incoming: &mut crate::types::TaskRecord,
    before: &crate::types::TaskRecord,
) -> Result<()> {
    use anyhow::Context;
    let cancelled = incoming.status == crate::types::TaskStatus::Cancelled;
    let incoming = incoming
        .detail
        .as_mut()
        .and_then(|detail| detail.as_object_mut())
        .context("Command terminal detail is not an object")?;
    for field in ["cancel_requested", "force_stop_requested"] {
        let frozen = incoming
            .get(field)
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let actual = before
            .detail
            .as_ref()
            .and_then(|detail| detail.get(field))
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if frozen || actual {
            incoming.insert(field.into(), serde_json::json!(true));
        }
    }
    if cancelled {
        // A Force accepted after the terminal body was frozen still wins in
        // the current Task, without replacing the original result message.
        let reason = if incoming
            .get("force_stop_requested")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            Some("force_stop_requested")
        } else if incoming
            .get("cancel_requested")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            Some("cancel_requested")
        } else {
            None
        };
        if let Some(reason) = reason {
            incoming.insert("cancelled_reason".into(), serde_json::json!(reason));
        }
    }
    Ok(())
}

pub(crate) struct OriginalCommandTaskInputView<'tx, 'connection, 'command> {
    tx: &'tx rusqlite::Transaction<'connection>,
    scope: &'command InputEvidenceScope,
    task: &'command crate::types::TaskRecord,
    message: &'command crate::types::MessageEnvelope,
}
impl<'tx, 'connection, 'command> OriginalCommandTaskInputView<'tx, 'connection, 'command> {
    pub(crate) fn prepare_tx(
        tx: &'tx rusqlite::Transaction<'connection>,
        source: &crate::runtime::OriginalCommandTaskProducer,
        proof: &super::task_snapshot::TaskWriteProof,
        task: &'command crate::types::TaskRecord,
        message: &'command crate::types::MessageEnvelope,
        intent: &'command OriginalCommandTaskTerminalIntent,
    ) -> Result<Self> {
        use anyhow::Context;
        let before = intent
            .before
            .as_ref()
            .context("Command derived Message lacks original before-version")?;
        proof.validate_task_before_tx(tx, &task.id)?;
        proof.validate_original_producer_before(
            &intent.original.scope,
            &task.id,
            &before.content_digest,
            &before.projection_digest,
        )?;
        let view = source.source_view()?;
        ensure!(
            view.address() == &intent.original && proof.scope() == &intent.original.scope,
            "Command derived Message source differs"
        );
        view.validate_original_generation(proof.task_before(&task.id)?)?;
        view.validate_original_generation(task)?;
        source.validate_terminal_message(task, message, &intent.terminal_digest)?;
        ensure!(
            super::TaskResultSettlementRecord::pending(task, message, message.created_at)?
                .is_none(),
            "Command no-rejoin bridge cannot substitute a Result obligation"
        );
        ensure!(
            message.agent_id == intent.original.scope.agent_id
                && message.task_id.as_deref() == Some(task.id.as_str())
                && matches!(&message.origin,crate::types::MessageOrigin::Task{task_id} if task_id==&task.id)
                && message.kind == crate::types::MessageKind::TaskResult
                && message.authority_class == crate::types::AuthorityClass::RuntimeInstruction
                && message.delivery_surface
                    == Some(crate::types::MessageDeliverySurface::TaskRejoin)
                && message.admission_context == Some(crate::types::AdmissionContext::RuntimeOwned),
            "Command derived Message provenance differs"
        );
        Ok(Self {
            tx,
            scope: &intent.original.scope,
            task,
            message,
        })
    }
    /// Same D3 derived-input bridge for the other closed accepted producer.
    /// A child-id/Task DTO cannot manufacture this source capability.
    pub(in crate::runtime_db) fn prepare_child_tx(
        tx: &'tx rusqlite::Transaction<'connection>,
        source: &crate::runtime::OriginalChildTaskProducer,
        proof: &super::task_snapshot::TaskWriteProof,
        task: &'command crate::types::TaskRecord,
        message: &'command crate::types::MessageEnvelope,
        scope: &'command InputEvidenceScope,
        before: &OriginalCommandTaskBefore,
        terminal_digest: &str,
        requested_digest: &str,
    ) -> Result<Self> {
        proof.validate_task_before_tx(tx, &task.id)?;
        proof.validate_original_producer_before(
            scope,
            &task.id,
            &before.content_digest,
            &before.projection_digest,
        )?;
        let view = source.source_view()?;
        ensure!(
            &view.address().scope == scope && proof.scope() == scope,
            "child derived input crossed original scope"
        );
        view.validate_generation(proof.task_before(&task.id)?)?;
        view.validate_generation(task)?;
        source.validate_terminal_message(task, message, terminal_digest)?;
        ensure!(
            super::input_mutation_receipts::canonical_command_digest(message)? == requested_digest,
            "child derived Message differs from its frozen enclosing command"
        );
        ensure!(
            super::TaskResultSettlementRecord::pending(task, message, message.created_at)?
                .is_none(),
            "child no-rejoin bridge cannot substitute a Result obligation"
        );
        ensure!(
            message.agent_id == scope.agent_id
                && message.task_id.as_deref() == Some(task.id.as_str())
                && matches!(&message.origin,crate::types::MessageOrigin::Task{task_id} if task_id==&task.id)
                && message.kind == crate::types::MessageKind::TaskResult
                && message.authority_class == crate::types::AuthorityClass::RuntimeInstruction
                && message.delivery_surface
                    == Some(crate::types::MessageDeliverySurface::TaskRejoin)
                && message.admission_context == Some(crate::types::AdmissionContext::RuntimeOwned),
            "child derived Message provenance differs"
        );
        Ok(Self {
            tx,
            scope,
            task,
            message,
        })
    }
    pub(crate) fn validate_tx(&self, tx: &rusqlite::Transaction<'_>) -> Result<()> {
        ensure!(
            std::ptr::eq(tx, self.tx),
            "Command derived Message belongs to another transaction"
        );
        Ok(())
    }
    pub(crate) fn scope(&self) -> &InputEvidenceScope {
        self.scope
    }
    pub(crate) fn task(&self) -> &crate::types::TaskRecord {
        self.task
    }
    pub(crate) fn message(&self) -> &crate::types::MessageEnvelope {
        self.message
    }
}

/// Integer-only bounds for the existing D4 binding reader, not Start authority.
/// Turn and checkpoint raw sizes conservatively cover selected JSON headers.
/// Each scalar length gate precedes any JSON projection of that source.
pub(crate) fn memory_binding_material_tx(
    tx: &rusqlite::Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &crate::types::AgentState,
    request: &super::task_snapshot::TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<Vec<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>> {
    use anyhow::Context;
    let Some(turn) = state.current_turn_id.as_deref() else {
        return Ok(Vec::new());
    };
    let turn_bytes: usize = tx.query_row(
        "SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(turn_id AS BLOB))+length(CAST(agent_id AS BLOB))+COALESCE(length(CAST(run_id AS BLOB)),0)+COALESCE(length(CAST(current_work_item_id AS BLOB)),0)+COALESCE(length(CAST(trigger_message_id AS BLOB)),0)+COALESCE(length(CAST(terminal_kind AS BLOB)),0)+length(CAST(created_at AS BLOB))+COALESCE(length(CAST(completed_at AS BLOB)),0) FROM turn_records WHERE turn_id=?1),0)",
        [turn],
        |row| row.get(0),
    )?;
    let turn_material = charge_material(
        scope,
        turn_bytes
            .checked_add(256)
            .and_then(|n| n.checked_mul(8))
            .context("Command original Turn material overflow")?,
        8,
        request,
        ledger,
    )?;
    if state.current_execution_binding.is_none() {
        return Ok(vec![turn_material]);
    }
    let message = state
        .current_execution_binding
        .as_ref()
        .map(|binding| binding.source_message_id.as_str());
    let checkpoint:usize=tx.query_row("SELECT COALESCE(SUM(length(CAST(payload_json AS BLOB))+length(CAST(state AS BLOB))+length(CAST(source_message_id AS BLOB))+length(CAST(seed_digest AS BLOB))),0) FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3",rusqlite::params![scope.agent_id,scope.identity_incarnation,turn],|r|r.get(0))?;
    let first = charge_material(
        scope,
        checkpoint
            .checked_add(1024)
            .and_then(|n| n.checked_mul(8))
            .context("Command Memory checkpoint material overflow")?,
        8,
        request,
        ledger,
    )?;
    let other:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(agent_id AS BLOB))+COALESCE(length(CAST(json_extract(payload_json,'$.run_id') AS BLOB)),0) FROM turn_records WHERE turn_id=?3),0)+COALESCE((SELECT length(CAST(agent_id AS BLOB))+length(CAST(kind AS BLOB)) FROM messages WHERE evidence_id=?4),0)+COALESCE((SELECT SUM(length(CAST(command_digest AS BLOB))+length(CAST(scope_digest AS BLOB))+length(CAST(prepared_json AS BLOB))) FROM input_mutation_receipts WHERE command_nonce IN (SELECT json_extract(payload_json,'$.original_start.command_nonce') FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3)),0)+COALESCE((SELECT SUM(length(CAST(command_digest AS BLOB))+length(CAST(invocation_json AS BLOB))+COALESCE(length(CAST(finalized_episode_id AS BLOB)),0)+length(CAST(result_json AS BLOB))+length(CAST(committed_at AS BLOB))) FROM episode_memory_results WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3),0)",rusqlite::params![scope.agent_id,scope.identity_incarnation,turn,message],|r|r.get(0))?;
    let second = charge_material(
        scope,
        other
            .checked_add(1024)
            .and_then(|n| n.checked_mul(8))
            .context("Command Memory binding material overflow")?,
        40,
        request,
        ledger,
    )?;
    Ok(vec![turn_material, first, second])
}

#[cfg(test)]
mod memory_material_tests {
    use super::*;
    use crate::{
        runtime::{
            input_evidence_budget::InputEvidenceBudgetLedger, resident_task::TaskCaptureRequest,
        },
        runtime_db::{
            repositories::upsert_turn_record_tx, task_snapshot::TaskDomainLoadRequest, RuntimeDb,
        },
        types::{AgentState, TurnRecord, WorkItemExecutionBinding},
    };

    #[test]
    fn original_command_turn_raw_material_precedes_json_in_both_binding_branches() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            directory.path().join("runtime.sqlite"),
            directory.path().join("runtime.lock"),
        )?;
        let scope = InputEvidenceScope {
            agent_id: "command-turn-material".into(),
            identity_incarnation: 1,
        };
        let mut turn = TurnRecord::new(&scope.agent_id, "original-command-turn", 1);
        turn.created_at = "2026-10-01T00:00:00Z".parse()?;
        turn.run_id = Some("original-command-run".into());
        db.transaction(|tx| {
            upsert_turn_record_tx(tx, &turn)?;
            tx.execute("INSERT INTO dispatch_memory_checkpoints(agent_id,identity_incarnation,turn_id,source_message_id,turn_index,state,seed_digest,record_digest,payload_json) VALUES(?1,1,?2,'original-command-message',1,'started',?3,?3,?4)", rusqlite::params![scope.agent_id,turn.turn_id,"a".repeat(64),r#"{"original_start":{"command_nonce":"original-command-start"}}"#])?;
            Ok(())
        })?;
        let raw: String = db.connection()?.query_row(
            "SELECT payload_json FROM turn_records WHERE turn_id=?1",
            [&turn.turn_id],
            |row| row.get(0),
        )?;
        let request = TaskDomainLoadRequest {
            recovery_at: None,
            query: Default::default(),
            capture: TaskCaptureRequest::current(),
            byte_limit: 4096,
            record_limit: 65536,
        };
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        for has_binding in [false, true] {
            let mut state = AgentState::new(&scope.agent_id);
            state.current_turn_id = Some(turn.turn_id.clone());
            state.current_run_id = turn.run_id.clone();
            if has_binding {
                state.current_execution_binding = Some(WorkItemExecutionBinding {
                    activation_id: None,
                    admission_provenance: None,
                    source_message_id: "original-command-message".into(),
                    turn_id: turn.turn_id.clone(),
                    owner: None,
                    work_item_id: None,
                    claimed_work_revision: None,
                });
            }
            db.connection()?.execute(
                "UPDATE turn_records SET payload_json=?1 WHERE turn_id=?2",
                rusqlite::params!["{".repeat(65536), turn.turn_id],
            )?;
            let error = db
                .transaction(|tx| memory_binding_material_tx(tx, &scope, &state, &request, &ledger))
                .err()
                .expect("oversized malformed original Turn must fail before JSON");
            assert!(
                matches!(
                    error.downcast_ref::<super::super::task_snapshot::TaskSnapshotError>(),
                    Some(super::super::task_snapshot::TaskSnapshotError::ResourceExhausted(_))
                ),
                "binding={has_binding}: {error:#}"
            );
            assert_eq!(ledger.report()?.observed_bytes, 0);
            db.connection()?.execute(
                "UPDATE turn_records SET payload_json=?1 WHERE turn_id=?2",
                rusqlite::params![raw, turn.turn_id],
            )?;
            let mut admitted = request.clone();
            admitted.byte_limit = 1024 * 1024;
            let guards = db.transaction(|tx| {
                memory_binding_material_tx(tx, &scope, &state, &admitted, &ledger)
            })?;
            assert!(ledger.report()?.observed_bytes >= u64::try_from(raw.len() * 8)?);
            drop(guards);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        Ok(())
    }
}

#[cfg(test)]
mod cancel_flag_tests {
    use super::*;
    use crate::types::{TaskKind, TaskRecord, TaskStatus};

    #[test]
    fn command_cancel_flag_merge_keeps_absent_false_shape_and_monotone_true() -> Result<()> {
        let at = "2026-10-01T00:00:00Z".parse()?;
        for field in ["cancel_requested", "force_stop_requested"] {
            for previous in [None, Some(false), Some(true)] {
                for frozen in [None, Some(false), Some(true)] {
                    let mut before = TaskRecord {
                        id: "original-command".into(),
                        agent_id: "original-owner".into(),
                        kind: TaskKind::CommandTask,
                        status: TaskStatus::Cancelling,
                        created_at: at,
                        updated_at: at,
                        parent_message_id: None,
                        work_item_id: None,
                        summary: None,
                        detail: Some(serde_json::json!({"kept":"original"})),
                        recovery: None,
                    };
                    if let Some(value) = previous {
                        before.detail.as_mut().unwrap()[field] = serde_json::json!(value);
                    }
                    let mut incoming = before.clone();
                    incoming.status = TaskStatus::Cancelled;
                    incoming.detail = Some(serde_json::json!({"kept":"terminal"}));
                    if let Some(value) = frozen {
                        incoming.detail.as_mut().unwrap()[field] = serde_json::json!(value);
                    }
                    merge_original_cancel_flags(&mut incoming, &before)?;
                    let expected = if previous == Some(true) || frozen == Some(true) {
                        Some(serde_json::json!(true))
                    } else {
                        frozen.map(|value| serde_json::json!(value))
                    };
                    assert_eq!(
                        incoming.detail.as_ref().unwrap().get(field),
                        expected.as_ref(),
                        "{field} previous={previous:?} frozen={frozen:?}"
                    );
                    assert_eq!(incoming.detail.as_ref().unwrap()["kept"], "terminal");
                    let other = if field == "cancel_requested" {
                        "force_stop_requested"
                    } else {
                        "cancel_requested"
                    };
                    assert!(incoming.detail.as_ref().unwrap().get(other).is_none());
                    let reason = expected
                        .as_ref()
                        .filter(|value| value.as_bool() == Some(true))
                        .map(|_| {
                            if field == "force_stop_requested" {
                                "force_stop_requested"
                            } else {
                                "cancel_requested"
                            }
                        });
                    assert_eq!(
                        incoming
                            .detail
                            .as_ref()
                            .unwrap()
                            .get("cancelled_reason")
                            .and_then(serde_json::Value::as_str),
                        reason
                    );
                }
            }
        }
        Ok(())
    }
}
