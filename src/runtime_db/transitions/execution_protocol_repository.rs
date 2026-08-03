use std::collections::BTreeMap;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, Transaction};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::domain::execution_protocol::{
    self, ExecutionProtocolCommand, ExecutionProtocolState, ExecutionTransition,
    WorkItemExecutionState,
};
use crate::{
    runtime_db::transitions::{ExecutionAuthorityFences, RuntimeTransitionRepository},
    types::{AgentIdentityRecord, AgentRegistryStatus, AgentStatus},
};

pub(super) struct PreparedExecutionProtocolCommands {
    state: ExecutionProtocolState,
    initialized_partition: bool,
    results: Vec<PreparedCommandResult>,
}

impl PreparedExecutionProtocolCommands {
    pub(super) fn has_writes(&self) -> bool {
        self.initialized_partition || !self.results.is_empty()
    }
}

struct PreparedCommandResult {
    command_kind: &'static str,
    command_identity: String,
    payload_hash: String,
    references: Vec<String>,
}

pub(super) fn validate_execution_commands_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    bootstrap: Option<&ExecutionProtocolState>,
    commands: &[ExecutionProtocolCommand],
) -> Result<Option<PreparedExecutionProtocolCommands>> {
    if commands.is_empty() {
        return Ok(None);
    }
    let initialized_partition = !partition_exists_tx(tx, agent_id)?;
    let mut state = if initialized_partition {
        let state = bootstrap
            .cloned()
            .ok_or_else(|| anyhow!("execution protocol command requires bootstrap state"))?;
        if state.agent_id != agent_id {
            bail!("execution protocol bootstrap agent does not match transaction partition");
        }
        execution_protocol::assert_invariants(&state)
            .map_err(|error| anyhow!("invalid execution protocol bootstrap: {error}"))?;
        state
    } else {
        load_state_tx(tx, agent_id)?
    };
    let mut results = Vec::new();

    for command in commands {
        let (command_kind, command_identity) = command_identity(command);
        let payload_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(command)?));
        if let Some(stored_hash) =
            stored_command_hash_tx(tx, agent_id, command_kind, command_identity)?
        {
            if stored_hash != payload_hash {
                bail!(
                    "execution protocol command identity conflict for agent {agent_id}, \
                     command {command_kind} {command_identity}"
                );
            }
            continue;
        }
        if let ExecutionProtocolCommand::Admit(command) = command {
            validate_admission_authority_tx(tx, agent_id, &command.attempt.admitted_fences)?;
        }
        let transition = reduce(&state, command)
            .map_err(|error| anyhow!("execution protocol command rejected: {error}"))?;
        state = transition.state;
        results.push(PreparedCommandResult {
            command_kind,
            command_identity: command_identity.to_owned(),
            payload_hash,
            references: transition.references,
        });
    }

    Ok(Some(PreparedExecutionProtocolCommands {
        state,
        initialized_partition,
        results,
    }))
}

pub(super) fn persist_execution_commands_tx(
    tx: &Transaction<'_>,
    prepared: Option<PreparedExecutionProtocolCommands>,
) -> Result<()> {
    let Some(prepared) = prepared else {
        return Ok(());
    };
    persist_state_tx(tx, &prepared.state)?;
    for result in prepared.results {
        tx.execute(
            "INSERT INTO execution_protocol_command_results (
               agent_id, command_kind, command_identity,
               payload_hash, references_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                prepared.state.agent_id,
                result.command_kind,
                result.command_identity,
                result.payload_hash,
                serde_json::to_string(&result.references)?,
                Utc::now().to_rfc3339(),
            ],
        )?;
    }
    Ok(())
}

fn reduce(
    state: &ExecutionProtocolState,
    command: &ExecutionProtocolCommand,
) -> Result<ExecutionTransition, String> {
    match command {
        ExecutionProtocolCommand::RegisterWorkItem(command) => {
            execution_protocol::register_work_item_execution(state, command)
        }
        ExecutionProtocolCommand::Admit(command) => {
            execution_protocol::admit_execution(state, command)
        }
        ExecutionProtocolCommand::Settle(command) => {
            execution_protocol::settle_execution(state, command)
        }
        ExecutionProtocolCommand::Interrupt(command) => {
            execution_protocol::interrupt_execution(state, command)
        }
    }
}

fn command_identity(command: &ExecutionProtocolCommand) -> (&'static str, &str) {
    match command {
        ExecutionProtocolCommand::RegisterWorkItem(command) => {
            ("register_work_item_execution", &command.work_item_id)
        }
        ExecutionProtocolCommand::Admit(command) => {
            ("admit_execution", &command.attempt.attempt_id)
        }
        ExecutionProtocolCommand::Settle(command) => {
            ("settle_execution", &command.outcome.outcome_id)
        }
        ExecutionProtocolCommand::Interrupt(command) => {
            ("interrupt_execution", &command.outcome_id)
        }
    }
}

fn partition_exists_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<bool> {
    tx.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM execution_protocol_partitions WHERE agent_id = ?1
         )",
        [agent_id],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

pub(crate) fn authority_fences_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<ExecutionAuthorityFences> {
    let (status, control_revision) = tx
        .query_row(
            "SELECT status, control_revision
             FROM agent_states
             WHERE agent_id = ?1",
            [agent_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow!("execution admission requires durable agent control state"))?;
    let status: AgentStatus =
        serde_json::from_str(&format!("\"{status}\"")).context("decoding agent control status")?;
    if status == AgentStatus::Stopped {
        bail!("execution admission rejected because agent control state is stopped");
    }
    let agent_control_revision =
        u64::try_from(control_revision).context("agent control revision is negative")?;
    if agent_control_revision == 0 {
        bail!("agent control revision must be nonzero");
    }

    let identity_payload = tx
        .query_row(
            "SELECT payload_json
             FROM agent_identities
             WHERE agent_id = ?1",
            [agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("execution admission requires durable host identity"))?;
    let identity: AgentIdentityRecord =
        serde_json::from_str(&identity_payload).context("decoding host identity")?;
    if identity.status != AgentRegistryStatus::Active {
        bail!("execution admission rejected because host identity is not active");
    }
    let host_registry_revision = identity
        .revision
        .checked_add(1)
        .ok_or_else(|| anyhow!("host identity revision overflow"))?;

    Ok(ExecutionAuthorityFences {
        agent_control_revision,
        host_registry_revision,
    })
}

fn validate_admission_authority_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    admitted: &execution_protocol::AdmittedFences,
) -> Result<()> {
    let current = authority_fences_tx(tx, agent_id)?;
    if admitted.agent_control_revision != current.agent_control_revision {
        bail!(
            "execution admission agent control fence is stale: admitted {}, current {}",
            admitted.agent_control_revision,
            current.agent_control_revision
        );
    }
    if admitted.host_registry_revision != current.host_registry_revision {
        bail!(
            "execution admission host registry fence is stale: admitted {}, current {}",
            admitted.host_registry_revision,
            current.host_registry_revision
        );
    }
    Ok(())
}

impl RuntimeTransitionRepository<'_> {
    pub(crate) fn load_execution_authority_fences(
        &self,
        agent_id: &str,
    ) -> Result<ExecutionAuthorityFences> {
        let connection = self.db.connection()?;
        let transaction =
            Transaction::new_unchecked(&connection, rusqlite::TransactionBehavior::Deferred)?;
        let fences = authority_fences_tx(&transaction, agent_id)?;
        transaction.commit()?;
        Ok(fences)
    }

    pub(crate) fn load_execution_protocol_state_if_initialized(
        &self,
        agent_id: &str,
    ) -> Result<Option<ExecutionProtocolState>> {
        let connection = self.db.connection()?;
        let transaction =
            Transaction::new_unchecked(&connection, rusqlite::TransactionBehavior::Deferred)?;
        if !partition_exists_tx(&transaction, agent_id)? {
            transaction.commit()?;
            return Ok(None);
        }
        let state = load_state_tx(&transaction, agent_id)?;
        transaction.commit()?;
        Ok(Some(state))
    }
}

fn stored_command_hash_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    command_kind: &str,
    command_identity: &str,
) -> Result<Option<String>> {
    tx.query_row(
        "SELECT payload_hash
         FROM execution_protocol_command_results
         WHERE agent_id = ?1
           AND command_kind = ?2
           AND command_identity = ?3",
        params![agent_id, command_kind, command_identity],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

pub(crate) fn persist_state_tx(tx: &Transaction<'_>, state: &ExecutionProtocolState) -> Result<()> {
    execution_protocol::assert_invariants(state)
        .map_err(|error| anyhow!("invalid execution protocol state: {error}"))?;
    tx.execute(
        "INSERT INTO execution_protocol_partitions (agent_id, updated_at)
         VALUES (?1, ?2)
         ON CONFLICT(agent_id) DO UPDATE SET updated_at = excluded.updated_at",
        params![state.agent_id, Utc::now().to_rfc3339()],
    )?;
    for (work_item_id, work_record) in &state.work_items {
        tx.execute(
            "INSERT INTO execution_protocol_work_items (
               agent_id, work_item_id, source_revision, generation,
               lifecycle_state, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(agent_id, work_item_id) DO UPDATE SET
               source_revision = excluded.source_revision,
               generation = excluded.generation,
               lifecycle_state = excluded.lifecycle_state,
               payload_json = excluded.payload_json",
            params![
                state.agent_id,
                work_item_id,
                to_i64(work_record.source_revision)?,
                to_i64(work_record.generation())?,
                work_item_state_token(&work_record.state),
                serde_json::to_string(work_record)?,
            ],
        )?;
    }
    for attempt in state.attempts.values() {
        tx.execute(
            "INSERT INTO execution_protocol_attempts (
               agent_id, attempt_id, lifecycle_state,
               source_identity_json, source_generation, recovery_of_attempt_id,
               terminal_outcome_id, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(agent_id, attempt_id) DO UPDATE SET
               lifecycle_state = excluded.lifecycle_state,
               terminal_outcome_id = excluded.terminal_outcome_id,
               payload_json = excluded.payload_json",
            params![
                state.agent_id,
                attempt.attempt_id,
                enum_token(&attempt.state)?,
                serde_json::to_string(&attempt.source.identity)?,
                to_i64(attempt.source.generation)?,
                attempt.recovery_of_attempt_id,
                attempt.terminal_outcome_id,
                serde_json::to_string(attempt)?,
            ],
        )?;
    }
    for outcome in state.outcomes.values() {
        let payload = serde_json::to_string(outcome)?;
        let existing = tx
            .query_row(
                "SELECT payload_json
                 FROM execution_protocol_outcomes
                 WHERE agent_id = ?1 AND outcome_id = ?2",
                params![state.agent_id, outcome.outcome_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing != payload {
                bail!(
                    "execution outcome identity conflict for {}",
                    outcome.outcome_id
                );
            }
            continue;
        }
        tx.execute(
            "INSERT INTO execution_protocol_outcomes (
               agent_id, outcome_id, attempt_id, payload_json
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                state.agent_id,
                outcome.outcome_id,
                outcome.attempt_id,
                payload,
            ],
        )?;
    }
    Ok(())
}

pub(super) fn load_state_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<ExecutionProtocolState> {
    if !partition_exists_tx(tx, agent_id)? {
        bail!("execution protocol partition for agent {agent_id} is not initialized");
    }
    let state = ExecutionProtocolState {
        agent_id: agent_id.to_owned(),
        attempts: load_payload_map(
            tx,
            "SELECT attempt_id, payload_json
             FROM execution_protocol_attempts WHERE agent_id = ?1",
            agent_id,
        )?,
        work_items: load_payload_map(
            tx,
            "SELECT work_item_id, payload_json
             FROM execution_protocol_work_items WHERE agent_id = ?1",
            agent_id,
        )?,
        outcomes: load_payload_map(
            tx,
            "SELECT outcome_id, payload_json
             FROM execution_protocol_outcomes WHERE agent_id = ?1",
            agent_id,
        )?,
    };
    execution_protocol::assert_invariants(&state)
        .map_err(|error| anyhow!("stored execution protocol state is invalid: {error}"))?;
    Ok(state)
}

fn load_payload_map<T: DeserializeOwned>(
    tx: &Transaction<'_>,
    sql: &str,
    agent_id: &str,
) -> Result<BTreeMap<String, T>> {
    let mut statement = tx.prepare(sql)?;
    let records = statement
        .query_map([agent_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .map(|row| {
            let (id, payload) = row?;
            let value = serde_json::from_str(&payload)
                .with_context(|| format!("decoding execution protocol record {id}"))?;
            Ok((id, value))
        })
        .collect();
    records
}

fn enum_token<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("execution protocol enum did not serialize to a string"))
}

fn work_item_state_token(state: &WorkItemExecutionState) -> &'static str {
    match state {
        WorkItemExecutionState::Runnable { .. } => "runnable",
        WorkItemExecutionState::InFlight { .. } => "in_flight",
        WorkItemExecutionState::Waiting { .. } => "waiting",
        WorkItemExecutionState::Paused { .. } => "paused",
        WorkItemExecutionState::NeedsRepair { .. } => "needs_repair",
        WorkItemExecutionState::Terminal { .. } => "terminal",
    }
}

fn to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).context("execution protocol generation exceeds SQLite INTEGER")
}
