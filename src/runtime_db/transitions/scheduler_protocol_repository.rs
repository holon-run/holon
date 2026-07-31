use crate::types::{
    WaitConditionRecord, WorkItemContinuationFrame, WorkItemPlanStatus, WorkItemRecord,
    WorkItemSchedulingState,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error as StdError,
    fmt,
};

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{inject_fault, RuntimeTransitionRepository, TransitionFaultPoint};
use crate::domain::scheduler_protocol::{
    self, ActivationCause, ActivationInputAttachment, ActivationRecord, ActivationSlot,
    ActivationState, AdmitActivationCommand, AdoptActivationWorkStateCommand,
    AdoptLegacyWorkStateCommand, AgentActivation, AgentDispatchState, ContinuationAdmissionRecord,
    Decision, LegacyWaitAdoption, MissingSettlementRecord, ProtocolCommand, ProtocolConflict,
    ProtocolConflictKind, ReplaceCompletedFocusProof, SchedulerOwner, Snapshot,
    WaitGenerationRecord, WaitIdentity, WaitRecord, WaitState, WaitTrigger, WorkDemand, WorkStatus,
};

const CANONICAL_COMMAND_SCHEMA_VERSION: i64 = 1;

#[derive(Serialize)]
struct LegacyActivationAuthorityPayload<'a> {
    authority_id: &'a str,
    activation: &'a AgentActivation,
    expected_scheduling_generation: u64,
    expected_dispatch_revision: u64,
    consumed_by: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LegacySchedulerAdoptionCandidate {
    pub agent_id: String,
    pub work_item_id: String,
    pub eligible: bool,
    pub reason: String,
    pub command: Option<ProtocolCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetiredSchedulerRolloutMetadata {
    pub retirement_marked: bool,
    pub protocol_mode: String,
    pub config_revision: u64,
    pub preflight_count: u64,
    pub manifest_count: u64,
    pub scenario_count: u64,
    pub authoritative_scenario_count: u64,
    pub stale_authoritative_scenario_count: u64,
    pub hard_blocker_count: u64,
    pub command_result_count: u64,
}

pub(super) struct PreparedProtocolCommands {
    snapshot: Snapshot,
    initialized_partition: bool,
    results: Vec<PreparedProtocolCommandResult>,
}

impl PreparedProtocolCommands {
    pub(super) fn has_writes(&self) -> bool {
        self.initialized_partition || !self.results.is_empty()
    }
}

struct PreparedProtocolCommandResult {
    command_kind: &'static str,
    command_identity: String,
    payload_hash: String,
    result: SchedulerProtocolCommandResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SchedulerProtocolCommandResult {
    pub decision: Decision,
    #[serde(default)]
    pub conflict: Option<ProtocolConflict>,
    pub transitions: Vec<String>,
    pub diagnostics: Vec<String>,
    pub fact_references: Vec<String>,
    pub pre_state_fence: serde_json::Value,
    pub post_state_fence: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchedulerProtocolTransitionCommit {
    pub applied: bool,
    pub replayed: bool,
    pub result: SchedulerProtocolCommandResult,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchedulerProtocolCommandIdentityConflict {
    pub conflict_attempt_id: i64,
    pub partition_kind: String,
    pub partition_key: String,
    pub command_kind: String,
    pub command_identity: String,
    pub existing_payload_hash: String,
    pub incoming_payload_hash: String,
    pub conflict: ProtocolConflict,
}

impl fmt::Display for SchedulerProtocolCommandIdentityConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "scheduler protocol command identity conflict attempt {} for {} {} {} {}: existing payload {}, incoming payload {} ({})",
            self.conflict_attempt_id,
            self.partition_kind,
            self.partition_key,
            self.command_kind,
            self.command_identity,
            self.existing_payload_hash,
            self.incoming_payload_hash,
            self.conflict.code,
        )
    }
}

impl StdError for SchedulerProtocolCommandIdentityConflict {}

#[derive(Debug, Serialize)]
struct SnapshotFence<'a> {
    slot: &'a ActivationSlot,
    dispatch: &'a AgentDispatchState,
    dispatch_revision: u64,
    focus: &'a Option<String>,
    work: BTreeMap<&'a str, WorkFence<'a>>,
}

#[derive(Debug, Serialize)]
struct WorkFence<'a> {
    metadata_revision: u64,
    scheduling_generation: u64,
    status: &'a WorkStatus,
}

#[derive(Debug)]
struct StoredCommandResult {
    payload_hash: String,
    result: SchedulerProtocolCommandResult,
}

enum CommandTransactionOutcome<T> {
    Commit(T),
    Conflict(SchedulerProtocolCommandIdentityConflict),
}

pub(super) fn validate_protocol_commands_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    bootstrap: Option<&Snapshot>,
    commands: &[ProtocolCommand],
) -> Result<Option<PreparedProtocolCommands>> {
    if commands.is_empty() {
        return Ok(None);
    }
    for command in commands {
        validate_command_agent(agent_id, command)?;
    }

    let initialized_partition = !scheduler_protocol_partition_exists_tx(tx, agent_id)?;
    let mut snapshot = if initialized_partition {
        let snapshot = bootstrap
            .cloned()
            .ok_or_else(|| anyhow!("canonical scheduler claim requires bootstrap state"))?;
        validate_agent_partition(agent_id, &snapshot)?;
        scheduler_protocol::assert_invariants(&snapshot)
            .map_err(|error| anyhow!("invalid scheduler protocol bootstrap: {error}"))?;
        snapshot
    } else {
        load_snapshot_tx(tx, agent_id)?
    };
    let mut results = Vec::new();

    for command in commands {
        let (command_kind, command_identity) = command_identity(command)?;
        let payload_hash = canonical_command_hash(command_kind, command)?;
        if let Some(stored) =
            stored_command_result_tx(tx, agent_id, command_kind, &command_identity)?
        {
            if stored.payload_hash != payload_hash {
                bail!(
                    "scheduler protocol command identity conflict for agent {}, command {} {}",
                    agent_id,
                    command_kind,
                    command_identity
                );
            }
            continue;
        }
        if let ProtocolCommand::AdoptLegacyWorkState(command) = command {
            validate_legacy_adoption_source_tx(tx, agent_id, command)?;
        }
        if let ProtocolCommand::AdoptActivationWorkState(command) = command {
            validate_activation_adoption_source_tx(tx, agent_id, command)?;
        }

        let pre_state_fence = snapshot_fence(&snapshot)?;
        let outcome = scheduler_protocol::reduce_command(&snapshot, command);
        scheduler_protocol::assert_invariants(&outcome.outcome.snapshot).map_err(|error| {
            anyhow!("scheduler protocol reducer produced invalid state: {error}")
        })?;
        if outcome.outcome.decision == Decision::Rejected {
            let code = outcome
                .conflict
                .as_ref()
                .map(|conflict| conflict.code.as_str())
                .or_else(|| outcome.outcome.diagnostics.first().map(String::as_str))
                .unwrap_or("rejected_without_diagnostic");
            bail!("canonical scheduler command {command_kind} rejected: {code}");
        }
        let post_state_fence = snapshot_fence(&outcome.outcome.snapshot)?;
        let decision = outcome.outcome.decision.clone();
        let result = SchedulerProtocolCommandResult {
            decision: decision.clone(),
            conflict: outcome.conflict,
            transitions: outcome.outcome.transitions,
            diagnostics: outcome.outcome.diagnostics,
            fact_references: decision_fact_references(&decision, command_fact_references(command)),
            pre_state_fence,
            post_state_fence,
        };
        snapshot = outcome.outcome.snapshot;
        results.push(PreparedProtocolCommandResult {
            command_kind,
            command_identity,
            payload_hash,
            result,
        });
    }

    Ok(Some(PreparedProtocolCommands {
        snapshot,
        initialized_partition,
        results,
    }))
}

pub(super) fn persist_protocol_commands_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    prepared: Option<PreparedProtocolCommands>,
) -> Result<()> {
    let Some(prepared) = prepared else {
        return Ok(());
    };
    if prepared.initialized_partition || !prepared.results.is_empty() {
        persist_agent_snapshot_tx(tx, agent_id, &prepared.snapshot)?;
    }
    for result in prepared.results {
        insert_command_result_tx(
            tx,
            agent_id,
            result.command_kind,
            &result.command_identity,
            &result.payload_hash,
            &result.result,
        )?;
    }
    Ok(())
}

impl RuntimeTransitionRepository<'_> {
    pub(crate) fn initialize_scheduler_protocol_partition(
        &self,
        agent_id: &str,
        snapshot: &Snapshot,
    ) -> Result<()> {
        validate_agent_partition(agent_id, snapshot)?;
        self.db.transaction(|tx| {
            if scheduler_protocol_partition_exists_tx(tx, agent_id)? {
                bail!("scheduler protocol partition for agent {agent_id} is already initialized");
            }
            scheduler_protocol::assert_invariants(snapshot)
                .map_err(|error| anyhow!("invalid scheduler protocol snapshot: {error}"))?;
            persist_agent_snapshot_tx(tx, agent_id, snapshot)?;
            Ok(())
        })
    }

    pub(crate) fn load_scheduler_protocol_snapshot(&self, agent_id: &str) -> Result<Snapshot> {
        self.load_scheduler_protocol_snapshot_with_hook(agent_id, || Ok(()))
    }

    pub(crate) fn load_scheduler_protocol_snapshot_if_initialized(
        &self,
        agent_id: &str,
    ) -> Result<Option<Snapshot>> {
        let connection = self.db.connection()?;
        let transaction = Transaction::new_unchecked(&connection, TransactionBehavior::Deferred)?;
        if !scheduler_protocol_partition_exists_tx(&transaction, agent_id)? {
            transaction.commit()?;
            return Ok(None);
        }
        let snapshot = load_snapshot_tx(&transaction, agent_id)?;
        transaction.commit()?;
        Ok(Some(snapshot))
    }

    pub(crate) fn inspect_retired_scheduler_rollout_metadata(
        &self,
    ) -> Result<RetiredSchedulerRolloutMetadata> {
        let connection = self.db.connection()?;
        let retirement_marked = connection.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM scheduler_rollout_retirement WHERE retirement_id = 1
             )",
            [],
            |row| row.get(0),
        )?;
        let (protocol_mode, config_revision): (String, i64) = connection.query_row(
            "SELECT protocol_mode, config_revision
             FROM scheduler_protocol_config
             WHERE config_id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let count = |sql: &str| -> Result<u64> {
            let value: i64 = connection.query_row(sql, [], |row| row.get(0))?;
            to_u64(value, "retired scheduler rollout metadata count")
        };
        Ok(RetiredSchedulerRolloutMetadata {
            retirement_marked,
            protocol_mode,
            config_revision: to_u64(config_revision, "rollout config revision")?,
            preflight_count: count("SELECT COUNT(*) FROM scheduler_rollout_preflights")?,
            manifest_count: count("SELECT COUNT(*) FROM scheduler_rollout_manifests")?,
            scenario_count: count("SELECT COUNT(*) FROM scheduler_scenario_authorities")?,
            authoritative_scenario_count: count(
                "SELECT COUNT(*) FROM scheduler_scenario_authorities
                 WHERE mode = 'authoritative'",
            )?,
            stale_authoritative_scenario_count: count(
                "SELECT COUNT(*)
                 FROM scheduler_scenario_authorities AS authority
                 LEFT JOIN scheduler_rollout_manifests AS manifest
                   ON manifest.manifest_revision = authority.manifest_revision
                 LEFT JOIN scheduler_rollout_preflights AS preflight
                   ON preflight.preflight_revision = authority.preflight_revision
                 WHERE authority.mode = 'authoritative'
                   AND (
                     authority.manifest_revision IS NULL
                     OR authority.preflight_revision IS NULL
                     OR manifest.manifest_revision IS NULL
                     OR preflight.preflight_revision IS NULL
                   )",
            )?,
            hard_blocker_count: count("SELECT COUNT(*) FROM scheduler_scenario_hard_blockers")?,
            command_result_count: count("SELECT COUNT(*) FROM scheduler_rollout_command_results")?,
        })
    }

    pub(crate) fn legacy_scheduler_adoption_candidates(
        &self,
        agent_id: &str,
    ) -> Result<Vec<LegacySchedulerAdoptionCandidate>> {
        self.db
            .transaction(|tx| legacy_scheduler_adoption_candidates_tx(tx, Some(agent_id)))
    }

    pub(crate) fn commit_scheduler_recovery_plan(
        &self,
        agent_id: &str,
        commands: &[ProtocolCommand],
    ) -> Result<bool> {
        if commands.is_empty() {
            return Ok(false);
        }
        self.db.transaction(|tx| {
            for command in commands {
                match command {
                    ProtocolCommand::AdoptLegacyWorkState(_)
                    | ProtocolCommand::AdoptActivationWorkState(_)
                    | ProtocolCommand::RegisterWorkDemand(_)
                    | ProtocolCommand::SettleActivation(_)
                    | ProtocolCommand::RecordMissingSettlement(_) => {}
                    ProtocolCommand::AdmitActivation(command)
                        if matches!(
                            command.activation.cause,
                            ActivationCause::SettlementRecovery { .. }
                        ) => {}
                    _ => bail!("scheduler recovery plan contains a non-recovery protocol command"),
                }
            }
            let bootstrap = (!scheduler_protocol_partition_exists_tx(tx, agent_id)?)
                .then(canonical_empty_snapshot);
            let prepared =
                validate_protocol_commands_tx(tx, agent_id, bootstrap.as_ref(), commands)?;
            let changed = prepared
                .as_ref()
                .is_some_and(PreparedProtocolCommands::has_writes);
            persist_protocol_commands_tx(tx, agent_id, prepared)?;
            Ok(changed)
        })
    }

    fn load_scheduler_protocol_snapshot_with_hook(
        &self,
        agent_id: &str,
        after_first_read: impl FnOnce() -> Result<()>,
    ) -> Result<Snapshot> {
        let connection = self.db.connection()?;
        let transaction = Transaction::new_unchecked(&connection, TransactionBehavior::Deferred)?;
        let snapshot =
            load_snapshot_connection_with_hook(&transaction, agent_id, after_first_read)?;
        transaction.commit()?;
        Ok(snapshot)
    }

    pub(crate) fn commit_scheduler_protocol_command(
        &self,
        agent_id: &str,
        command: &ProtocolCommand,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<SchedulerProtocolTransitionCommit> {
        self.commit_scheduler_protocol_command_inner(agent_id, command, fault)
    }

    #[cfg(test)]
    pub(crate) fn commit_scheduler_protocol_command_unchecked_for_test(
        &self,
        agent_id: &str,
        command: &ProtocolCommand,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<SchedulerProtocolTransitionCommit> {
        self.commit_scheduler_protocol_command_inner(agent_id, command, fault)
    }

    fn commit_scheduler_protocol_command_inner(
        &self,
        agent_id: &str,
        command: &ProtocolCommand,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<SchedulerProtocolTransitionCommit> {
        validate_command_agent(agent_id, command)?;
        let (command_kind, command_identity) = command_identity(command)?;
        let payload_hash = canonical_command_hash(command_kind, command)?;

        let outcome = self.db.transaction(|tx| {
            if let Some(stored) =
                stored_command_result_tx(tx, agent_id, command_kind, &command_identity)?
            {
                if stored.payload_hash != payload_hash {
                    let conflict = insert_command_identity_conflict_attempt_tx(
                        tx,
                        "agent",
                        agent_id,
                        command_kind,
                        &command_identity,
                        &stored.payload_hash,
                        &payload_hash,
                    )?;
                    return Ok(CommandTransactionOutcome::Conflict(conflict));
                }
                return Ok(CommandTransactionOutcome::Commit(
                    SchedulerProtocolTransitionCommit {
                        applied: false,
                        replayed: true,
                        result: stored.result,
                    },
                ));
            }

            let snapshot = load_snapshot_tx(tx, agent_id)?;
            if let ProtocolCommand::AdoptLegacyWorkState(command) = command {
                validate_legacy_adoption_source_tx(tx, agent_id, command)?;
            }
            if let ProtocolCommand::AdoptActivationWorkState(command) = command {
                validate_activation_adoption_source_tx(tx, agent_id, command)?;
            }
            let outcome = scheduler_protocol::reduce_command(&snapshot, command);
            scheduler_protocol::assert_invariants(&outcome.outcome.snapshot).map_err(|error| {
                anyhow!("scheduler protocol reducer produced invalid state: {error}")
            })?;
            inject_fault(fault, TransitionFaultPoint::AfterValidation)?;

            persist_agent_snapshot_tx(tx, agent_id, &outcome.outcome.snapshot)?;
            inject_fault(fault, TransitionFaultPoint::AfterCanonicalWrites)?;

            let decision = outcome.outcome.decision.clone();
            let result = SchedulerProtocolCommandResult {
                decision: decision.clone(),
                conflict: outcome.conflict,
                transitions: outcome.outcome.transitions,
                diagnostics: outcome.outcome.diagnostics,
                fact_references: decision_fact_references(
                    &decision,
                    command_fact_references(command),
                ),
                pre_state_fence: snapshot_fence(&snapshot)?,
                post_state_fence: snapshot_fence(&outcome.outcome.snapshot)?,
            };
            insert_command_result_tx(
                tx,
                agent_id,
                command_kind,
                &command_identity,
                &payload_hash,
                &result,
            )?;
            inject_fault(fault, TransitionFaultPoint::BeforeCommit)?;

            Ok(CommandTransactionOutcome::Commit(
                SchedulerProtocolTransitionCommit {
                    applied: true,
                    replayed: false,
                    result,
                },
            ))
        })?;
        match outcome {
            CommandTransactionOutcome::Commit(commit) => Ok(commit),
            CommandTransactionOutcome::Conflict(conflict) => Err(conflict.into()),
        }
    }
}

fn legacy_scheduler_adoption_candidates_tx(
    tx: &Transaction<'_>,
    agent_filter: Option<&str>,
) -> Result<Vec<LegacySchedulerAdoptionCandidate>> {
    let mut statement = tx.prepare(
        "SELECT payload_json
         FROM work_items
         WHERE state = 'open'
           AND (?1 IS NULL OR agent_id = ?1)
           AND NOT EXISTS (
               SELECT 1
               FROM agent_identities
               WHERE agent_identities.agent_id = work_items.agent_id
                 AND agent_identities.status IN ('deleting', 'deleted', 'archived')
           )
         ORDER BY agent_id, work_item_id",
    )?;
    let work_items = statement
        .query_map([agent_filter], |row| row.get::<_, String>(0))?
        .map(|row| crate::runtime_db::repositories::decode_work_item_payload(&row?))
        .collect::<Result<Vec<_>>>()?;
    let mut candidates = Vec::with_capacity(work_items.len());
    for work_item in work_items {
        candidates.push(legacy_scheduler_adoption_candidate_tx(tx, &work_item)?);
    }
    Ok(candidates)
}

fn legacy_scheduler_adoption_candidate_tx(
    tx: &Transaction<'_>,
    work_item: &WorkItemRecord,
) -> Result<LegacySchedulerAdoptionCandidate> {
    let agent_state = tx
        .query_row(
            "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
            [&work_item.agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| crate::runtime_db::repositories::decode_agent_state_payload(&payload))
        .transpose()?;
    if agent_state.as_ref().is_some_and(|state| {
        state.current_run_id.is_some()
            || matches!(state.status, crate::types::AgentStatus::AwakeRunning)
    }) {
        return Ok(ineligible_legacy_adoption(
            work_item,
            "legacy_agent_turn_running",
        ));
    }
    let dequeued = tx.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM queue_entries
             WHERE agent_id = ?1 AND status = 'dequeued'
         )",
        [&work_item.agent_id],
        |row| row.get::<_, bool>(0),
    )?;
    if dequeued {
        return Ok(ineligible_legacy_adoption(
            work_item,
            "legacy_queue_claim_in_progress",
        ));
    }

    let mut wait_statement = tx.prepare(
        "SELECT payload_json
         FROM wait_conditions
         WHERE agent_id = ?1 AND work_item_id = ?2 AND status = 'active'
         ORDER BY created_at, wait_condition_id",
    )?;
    let active_waits = wait_statement
        .query_map(
            [work_item.agent_id.as_str(), work_item.id.as_str()],
            |row| row.get::<_, String>(0),
        )?
        .map(|row| serde_json::from_str::<WaitConditionRecord>(&row?).map_err(Into::into))
        .collect::<Result<Vec<_>>>()?;
    if active_waits.len() > 1 {
        return Ok(ineligible_legacy_adoption(
            work_item,
            "multiple_active_legacy_waits",
        ));
    }

    let mut continuation_statement = tx.prepare(
        "SELECT payload_json
         FROM work_item_continuations
         WHERE agent_id = ?1 AND suspended_work_item_id = ?2 AND state = 'active'",
    )?;
    let continuations = continuation_statement
        .query_map(
            [work_item.agent_id.as_str(), work_item.id.as_str()],
            |row| row.get::<_, String>(0),
        )?
        .map(|row| crate::runtime_db::repositories::decode_work_item_continuation_payload(&row?))
        .collect::<Result<Vec<WorkItemContinuationFrame>>>()?;
    if !continuations.is_empty() {
        return Ok(ineligible_legacy_adoption(
            work_item,
            "legacy_yielded_work_item_requires_settlement_history",
        ));
    }

    let is_current = agent_state
        .as_ref()
        .and_then(|state| state.current_work_item_id.as_deref())
        == Some(work_item.id.as_str());
    let trigger_delivery_by_id = BTreeMap::new();
    let scheduling = crate::work_item_scheduling::derive_work_item_scheduling(
        crate::work_item_scheduling::WorkItemSchedulingFacts {
            work_item,
            is_current,
            is_yielded: false,
            active_wait_conditions: &active_waits,
            trigger_delivery_by_id: &trigger_delivery_by_id,
        },
    );
    let generation = work_item.revision.max(1);
    let (status, wait) = match scheduling.scheduling_state {
        WorkItemSchedulingState::Runnable => (WorkStatus::Runnable, None),
        WorkItemSchedulingState::WaitingTask
        | WorkItemSchedulingState::WaitingExternal
        | WorkItemSchedulingState::WaitingTimer
        | WorkItemSchedulingState::WaitingSystem
        | WorkItemSchedulingState::WaitingOperator
            if active_waits.len() == 1 =>
        {
            let wait = &active_waits[0];
            (
                WorkStatus::Waiting {
                    wait_id: wait.id.clone(),
                },
                Some(LegacyWaitAdoption {
                    wait_id: wait.id.clone(),
                    generation,
                    owner_work_item_id: work_item.id.clone(),
                    source_updated_at: wait.updated_at.to_rfc3339(),
                }),
            )
        }
        WorkItemSchedulingState::WaitingOperator
            if work_item.plan_status == WorkItemPlanStatus::NeedsInput =>
        {
            (
                WorkStatus::Paused {
                    hold_id: format!("legacy-plan-needs-input:{}", work_item.id),
                },
                None,
            )
        }
        WorkItemSchedulingState::Blocked => {
            return Ok(ineligible_legacy_adoption(
                work_item,
                "legacy_blocked_work_item_requires_operator_resolution",
            ));
        }
        WorkItemSchedulingState::YieldedToWorkItem => {
            return Ok(ineligible_legacy_adoption(
                work_item,
                "legacy_yielded_work_item_requires_settlement_history",
            ));
        }
        WorkItemSchedulingState::Completed => {
            return Ok(ineligible_legacy_adoption(
                work_item,
                "legacy_open_work_item_projects_completed",
            ));
        }
        _ => {
            return Ok(ineligible_legacy_adoption(
                work_item,
                "legacy_waiting_work_item_has_no_unique_active_wait",
            ));
        }
    };
    let reserve_dispatch = wait.is_some()
        && is_current
        && agent_state.as_ref().is_some_and(|state| {
            matches!(
                state.status,
                crate::types::AgentStatus::AwaitingTask | crate::types::AgentStatus::Asleep
            )
        });
    let mut command = ProtocolCommand::AdoptLegacyWorkState(AdoptLegacyWorkStateCommand {
        work_item_id: work_item.id.clone(),
        source_work_item_revision: work_item.revision,
        demand: WorkDemand {
            metadata_revision: work_item.revision,
            scheduling_generation: generation,
            status,
            capabilities: Default::default(),
            locks: Default::default(),
            locality: "runtime".into(),
            cost_class: "default".into(),
        },
        wait,
        focus: is_current,
        reserve_dispatch,
        replace_completed_focus: None,
    });
    // Check for a stale canonical focus that can be provably replaced.
    if is_current && scheduler_protocol_partition_exists_tx(tx, &work_item.agent_id)? {
        let snapshot = load_snapshot_tx(tx, &work_item.agent_id)?;
        if let Some(stale_focus_id) = snapshot.focus.as_deref() {
            if stale_focus_id != work_item.id {
                if let Some(proof) = build_replace_completed_focus_proof_tx(
                    tx,
                    &work_item.agent_id,
                    stale_focus_id,
                    &snapshot,
                )? {
                    if let ProtocolCommand::AdoptLegacyWorkState(ref mut cmd) = command {
                        cmd.replace_completed_focus = Some(proof);
                    }
                }
            }
        }
    }
    Ok(LegacySchedulerAdoptionCandidate {
        agent_id: work_item.agent_id.clone(),
        work_item_id: work_item.id.clone(),
        eligible: true,
        reason: "eligible".into(),
        command: Some(command),
    })
}

/// Builds a proof that the stale canonical focus can be safely replaced because
/// its legacy WorkItem is provably completed and its canonical demand is in a
/// safe state. Returns `None` if any condition is not met (fail-closed).
fn build_replace_completed_focus_proof_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    stale_focus_id: &str,
    snapshot: &Snapshot,
) -> Result<Option<ReplaceCompletedFocusProof>> {
    // The old focus must be a completed legacy WorkItem of the same agent.
    let stale_payload = tx
        .query_row(
            "SELECT payload_json FROM work_items
             WHERE work_item_id = ?1 AND agent_id = ?2 AND state = 'completed'",
            [stale_focus_id, agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let stale_work_item = match stale_payload {
        Some(payload) => crate::runtime_db::repositories::decode_work_item_payload(&payload)?,
        None => return Ok(None),
    };
    // The old focus demand must exist in the canonical snapshot.
    let old_demand = match snapshot.work.get(stale_focus_id) {
        Some(demand) => demand,
        None => return Ok(None),
    };
    // Safe states only: Runnable or Paused.
    if !matches!(
        old_demand.status,
        WorkStatus::Runnable | WorkStatus::Paused { .. }
    ) {
        return Ok(None);
    }
    // Activation slot must not be occupied by the old focus.
    if let ActivationSlot::Running { owner, .. } = &snapshot.slot {
        if owner.work_item_id() == Some(stale_focus_id) {
            return Ok(None);
        }
    }
    Ok(Some(ReplaceCompletedFocusProof {
        work_item_id: stale_focus_id.to_string(),
        source_work_item_revision: stale_work_item.revision,
        expected_metadata_revision: old_demand.metadata_revision,
        expected_scheduling_generation: old_demand.scheduling_generation,
    }))
}

fn ineligible_legacy_adoption(
    work_item: &WorkItemRecord,
    reason: &str,
) -> LegacySchedulerAdoptionCandidate {
    LegacySchedulerAdoptionCandidate {
        agent_id: work_item.agent_id.clone(),
        work_item_id: work_item.id.clone(),
        eligible: false,
        reason: reason.into(),
        command: None,
    }
}

fn validate_legacy_adoption_source_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    command: &AdoptLegacyWorkStateCommand,
) -> Result<()> {
    let work_item = tx
        .query_row(
            "SELECT payload_json FROM work_items
             WHERE work_item_id = ?1 AND agent_id = ?2 AND state = 'open'",
            [command.work_item_id.as_str(), agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| crate::runtime_db::repositories::decode_work_item_payload(&payload))
        .transpose()?
        .ok_or_else(|| anyhow!("legacy adoption source WorkItem is missing or closed"))?;
    let candidate = legacy_scheduler_adoption_candidate_tx(tx, &work_item)?;
    if !candidate.eligible
        || candidate.command.as_ref()
            != Some(&ProtocolCommand::AdoptLegacyWorkState(command.clone()))
    {
        bail!(
            "legacy adoption source changed for {}:{} ({})",
            agent_id,
            command.work_item_id,
            candidate.reason
        );
    }
    Ok(())
}

fn validate_activation_adoption_source_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    command: &AdoptActivationWorkStateCommand,
) -> Result<()> {
    let work_item = tx
        .query_row(
            "SELECT payload_json FROM work_items
             WHERE work_item_id = ?1 AND agent_id = ?2 AND state = 'open'",
            [command.work_item_id.as_str(), agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| crate::runtime_db::repositories::decode_work_item_payload(&payload))
        .transpose()?
        .ok_or_else(|| anyhow!("activation adoption source WorkItem is missing or closed"))?;
    if work_item.revision != command.source_work_item_revision {
        bail!(
            "activation adoption WorkItem source changed for {}:{}",
            agent_id,
            command.work_item_id
        );
    }
    let (wait_turn_id, wait_updated_at) = tx
        .query_row(
            "SELECT last_turn_id, updated_at FROM wait_conditions
             WHERE wait_condition_id = ?1
               AND agent_id = ?2
               AND work_item_id = ?3
               AND status = 'active'",
            [
                command.wait.wait_id.as_str(),
                agent_id,
                command.work_item_id.as_str(),
            ],
            |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow!("activation adoption source wait is missing or inactive"))?;
    if wait_turn_id.as_deref() != Some(command.source_turn_id.as_str()) {
        bail!(
            "activation adoption wait turn changed for {}:{}",
            agent_id,
            command.wait.wait_id
        );
    }
    if crate::runtime_db::repositories::parse_timestamp(&wait_updated_at)?
        != crate::runtime_db::repositories::parse_timestamp(&command.wait.source_updated_at)?
    {
        bail!(
            "activation adoption wait revision changed for {}:{}",
            agent_id,
            command.wait.wait_id
        );
    }
    let (owner_kind, owner_id, admitted_generation) = tx
        .query_row(
            "SELECT owner_kind, owner_id, admitted_generation
             FROM scheduler_activations
             WHERE agent_id = ?1 AND activation_id = ?2",
            [agent_id, command.source_activation_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("activation adoption source activation is missing"))?;
    if command.source_activation_id != format!("activation:message:{}", command.source_message_id)
        || to_u64(
            admitted_generation,
            "activation adoption admitted generation",
        )? != command.source_admitted_generation
        || scheduler_owner_from_columns(&owner_kind, owner_id, agent_id)?
            != (SchedulerOwner::AgentLifecycle {
                agent_id: agent_id.to_string(),
            })
    {
        bail!(
            "activation adoption source activation changed for {}:{}",
            agent_id,
            command.source_activation_id
        );
    }
    Ok(())
}

fn decision_fact_references(decision: &Decision, references: Vec<String>) -> Vec<String> {
    if *decision == Decision::Rejected {
        Vec::new()
    } else {
        references
    }
}

fn canonical_empty_snapshot() -> Snapshot {
    Snapshot {
        slot: ActivationSlot::Idle,
        dispatch: AgentDispatchState::Open,
        dispatch_revision: 0,
        focus: None,
        work: BTreeMap::new(),
        waits: BTreeMap::new(),
        activations: BTreeMap::new(),
        activation_admissions: BTreeMap::new(),
        settlements: BTreeMap::new(),
        missing_settlements: BTreeMap::new(),
        admitted_generations: BTreeSet::new(),
        continuation_admissions: BTreeMap::new(),
        activation_inputs: BTreeMap::new(),
    }
}

fn snapshot_fence(snapshot: &Snapshot) -> Result<serde_json::Value> {
    let work = snapshot
        .work
        .iter()
        .map(|(work_item_id, demand)| {
            (
                work_item_id.as_str(),
                WorkFence {
                    metadata_revision: demand.metadata_revision,
                    scheduling_generation: demand.scheduling_generation,
                    status: &demand.status,
                },
            )
        })
        .collect();
    Ok(serde_json::to_value(SnapshotFence {
        slot: &snapshot.slot,
        dispatch: &snapshot.dispatch,
        dispatch_revision: snapshot.dispatch_revision,
        focus: &snapshot.focus,
        work,
    })?)
}

fn command_identity(command: &ProtocolCommand) -> Result<(&'static str, String)> {
    Ok(match command {
        ProtocolCommand::RegisterWorkDemand(command) => {
            ("register_work_demand", command.work_item_id.clone())
        }
        ProtocolCommand::AdoptLegacyWorkState(command) => (
            "adopt_legacy_work_state",
            format!(
                "{}:{}",
                command.work_item_id, command.source_work_item_revision
            ),
        ),
        ProtocolCommand::AdoptActivationWorkState(command) => (
            "adopt_activation_work_state",
            format!("{}:{}", command.source_activation_id, command.work_item_id),
        ),
        ProtocolCommand::AdmitActivation(command) => {
            ("admit_activation", command.activation.id.clone())
        }
        ProtocolCommand::SettleActivation(command) => {
            ("settle_activation", command.settlement.id.clone())
        }
        ProtocolCommand::RecordMissingSettlement(record) => {
            ("record_missing_settlement", record.id.clone())
        }
        ProtocolCommand::TriggerWait(command) => (
            "trigger_wait",
            serde_json::to_string(&(command.wait_id.as_str(), command.wait_generation))?,
        ),
        ProtocolCommand::AttachActivationInput(command) => {
            ("attach_activation_input", command.attachment.id.clone())
        }
    })
}

fn canonical_command_hash(command_kind: &str, command: &ProtocolCommand) -> Result<String> {
    let canonical = serde_json::to_vec(&serde_json::json!({
        "schema_version": CANONICAL_COMMAND_SCHEMA_VERSION,
        "command_kind": command_kind,
        "command": command,
    }))?;
    Ok(format!("sha256:{:x}", Sha256::digest(canonical)))
}

fn command_fact_references(command: &ProtocolCommand) -> Vec<String> {
    match command {
        ProtocolCommand::RegisterWorkDemand(command) => {
            vec![format!("work:{}", command.work_item_id)]
        }
        ProtocolCommand::AdoptLegacyWorkState(command) => {
            let mut references = vec![format!("work:{}", command.work_item_id)];
            if let Some(wait) = &command.wait {
                references.push(format!(
                    "wait:{}:generation:{}",
                    wait.wait_id, wait.generation
                ));
            }
            if let Some(proof) = &command.replace_completed_focus {
                references.push(format!("work:{}:legacy_focus_replaced", proof.work_item_id));
            }
            references
        }
        ProtocolCommand::AdoptActivationWorkState(command) => vec![
            format!("activation:{}", command.source_activation_id),
            format!("work:{}", command.work_item_id),
            format!(
                "wait:{}:generation:{}",
                command.wait.wait_id, command.wait.generation
            ),
        ],
        ProtocolCommand::AdmitActivation(command) => vec![
            format!("activation:{}", command.activation.id),
            format!("activation_authority:{}", command.authority_id),
        ],
        ProtocolCommand::SettleActivation(command) => vec![
            format!("activation_settlement:{}", command.settlement.id),
            format!("activation:{}", command.settlement.activation_id),
        ],
        ProtocolCommand::RecordMissingSettlement(record) => vec![
            format!("missing_settlement:{}", record.id),
            format!("activation:{}", record.activation_id),
        ],
        ProtocolCommand::TriggerWait(command) => vec![format!(
            "wait:{}:generation:{}",
            command.wait_id, command.wait_generation
        )],
        ProtocolCommand::AttachActivationInput(command) => vec![
            format!("activation:{}", command.attachment.activation_id),
            format!("activation_input:{}", command.attachment.id),
        ],
    }
}

fn validate_command_agent(agent_id: &str, command: &ProtocolCommand) -> Result<()> {
    let command_agent_id = match command {
        ProtocolCommand::RegisterWorkDemand(_)
        | ProtocolCommand::AdoptLegacyWorkState(_)
        | ProtocolCommand::AdoptActivationWorkState(_) => None,
        ProtocolCommand::AdmitActivation(command) => Some(&command.activation.agent_id),
        ProtocolCommand::SettleActivation(_)
        | ProtocolCommand::RecordMissingSettlement(_)
        | ProtocolCommand::TriggerWait(_)
        | ProtocolCommand::AttachActivationInput(_) => None,
    };
    if command_agent_id.is_some_and(|command_agent_id| command_agent_id != agent_id) {
        bail!("scheduler protocol command crosses agent partition {agent_id}");
    }
    Ok(())
}

fn validate_agent_partition(agent_id: &str, snapshot: &Snapshot) -> Result<()> {
    if agent_id.is_empty() {
        bail!("scheduler protocol partition requires a non-empty agent id");
    }
    for admission in snapshot.activation_admissions.values() {
        if admission.activation.agent_id != agent_id {
            bail!(
                "activation admission {} belongs to another agent",
                admission.activation.id
            );
        }
    }
    Ok(())
}

fn scheduler_protocol_partition_exists_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<bool> {
    for table in [
        "scheduler_agent_slots",
        "scheduler_agent_dispatch",
        "scheduler_agent_focus",
        "scheduler_work_demands",
    ] {
        let sql = format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE agent_id = ?1)");
        if tx.query_row(&sql, [agent_id], |row| row.get::<_, bool>(0))? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn stored_command_result_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    command_kind: &str,
    command_identity: &str,
) -> Result<Option<StoredCommandResult>> {
    tx.query_row(
        "SELECT
           payload_hash,
           decision,
           conflict_kind,
           conflict_code,
           result_references_json,
           pre_state_fence_json,
           post_state_fence_json,
           outcome_json
         FROM scheduler_protocol_command_results
         WHERE agent_id = ?1 AND command_kind = ?2 AND command_identity = ?3",
        params![agent_id, command_kind, command_identity],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        },
    )
    .optional()?
    .map(
        |(
            payload_hash,
            decision,
            conflict_kind,
            conflict_code,
            result_references_json,
            pre_state_fence_json,
            post_state_fence_json,
            outcome_json,
        )| {
            let mut result: SchedulerProtocolCommandResult = serde_json::from_str(&outcome_json)?;
            let stored_decision = enum_token(&result.decision)?;
            if decision != stored_decision {
                bail!("stored scheduler protocol decision column disagrees with outcome");
            }
            let stored_conflict = result
                .conflict
                .as_ref()
                .map(|conflict| {
                    Ok::<_, anyhow::Error>((enum_token(&conflict.kind)?, conflict.code.clone()))
                })
                .transpose()?;
            if stored_conflict
                != conflict_kind
                    .zip(conflict_code)
                    .map(|(kind, code)| (kind, code))
            {
                bail!("stored scheduler protocol conflict columns disagree with outcome");
            }
            result.fact_references = serde_json::from_str(&result_references_json)?;
            result.pre_state_fence = serde_json::from_str(&pre_state_fence_json)?;
            result.post_state_fence = serde_json::from_str(&post_state_fence_json)?;
            Ok(StoredCommandResult {
                payload_hash,
                result,
            })
        },
    )
    .transpose()
}

fn insert_command_identity_conflict_attempt_tx(
    tx: &Transaction<'_>,
    partition_kind: &str,
    partition_key: &str,
    command_kind: &str,
    command_identity: &str,
    existing_payload_hash: &str,
    incoming_payload_hash: &str,
) -> Result<SchedulerProtocolCommandIdentityConflict> {
    let conflict = ProtocolConflict {
        kind: ProtocolConflictKind::PayloadConflict,
        code: "command_identity_payload_conflict".into(),
    };
    tx.execute(
        "INSERT INTO scheduler_protocol_command_conflict_attempts (
           partition_kind,
           partition_key,
           command_kind,
           command_identity,
           canonical_schema_version,
           existing_payload_hash,
           incoming_payload_hash,
           conflict_kind,
           conflict_code,
           created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            partition_kind,
            partition_key,
            command_kind,
            command_identity,
            CANONICAL_COMMAND_SCHEMA_VERSION,
            existing_payload_hash,
            incoming_payload_hash,
            enum_token(&conflict.kind)?,
            &conflict.code,
            Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(SchedulerProtocolCommandIdentityConflict {
        conflict_attempt_id: tx.last_insert_rowid(),
        partition_kind: partition_kind.to_string(),
        partition_key: partition_key.to_string(),
        command_kind: command_kind.to_string(),
        command_identity: command_identity.to_string(),
        existing_payload_hash: existing_payload_hash.to_string(),
        incoming_payload_hash: incoming_payload_hash.to_string(),
        conflict,
    })
}

fn insert_command_result_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    command_kind: &str,
    command_identity: &str,
    payload_hash: &str,
    result: &SchedulerProtocolCommandResult,
) -> Result<()> {
    let conflict_kind = result
        .conflict
        .as_ref()
        .map(|conflict| enum_token(&conflict.kind))
        .transpose()?;
    let conflict_code = result
        .conflict
        .as_ref()
        .map(|conflict| conflict.code.as_str());
    tx.execute(
        "INSERT INTO scheduler_protocol_command_results (
           agent_id,
           command_kind,
           command_identity,
           canonical_schema_version,
           payload_hash,
           decision,
           conflict_kind,
           conflict_code,
           result_references_json,
           pre_state_fence_json,
           post_state_fence_json,
           outcome_json,
           created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            agent_id,
            command_kind,
            command_identity,
            CANONICAL_COMMAND_SCHEMA_VERSION,
            payload_hash,
            enum_token(&result.decision)?,
            conflict_kind,
            conflict_code,
            serde_json::to_string(&result.fact_references)?,
            serde_json::to_string(&result.pre_state_fence)?,
            serde_json::to_string(&result.post_state_fence)?,
            serde_json::to_string(result)?,
            Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn enum_token<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("expected scheduler protocol enum to serialize as a string"))
}

fn persist_agent_snapshot_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    snapshot: &Snapshot,
) -> Result<()> {
    validate_agent_partition(agent_id, snapshot)?;
    scheduler_protocol::assert_invariants(snapshot)
        .map_err(|error| anyhow!("invalid scheduler protocol snapshot: {error}"))?;
    let now = Utc::now().to_rfc3339();

    for (work_item_id, demand) in &snapshot.work {
        let (status, status_reference_id) = work_status_columns(&demand.status);
        let staged_status = if matches!(demand.status, WorkStatus::Terminal) {
            "runnable"
        } else {
            status
        };
        tx.execute(
            "INSERT INTO scheduler_work_demands (
               agent_id,
               work_item_id,
               metadata_revision,
               scheduling_generation,
               status,
               status_reference_id,
               capabilities_json,
               locks_json,
               locality,
               cost_class,
               payload_json,
               updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(agent_id, work_item_id) DO UPDATE SET
               metadata_revision = excluded.metadata_revision,
               scheduling_generation = excluded.scheduling_generation,
               status = excluded.status,
               status_reference_id = excluded.status_reference_id,
               capabilities_json = excluded.capabilities_json,
               locks_json = excluded.locks_json,
               locality = excluded.locality,
               cost_class = excluded.cost_class,
               payload_json = excluded.payload_json,
               updated_at = excluded.updated_at",
            params![
                agent_id,
                work_item_id,
                to_i64(demand.metadata_revision, "work metadata revision")?,
                to_i64(demand.scheduling_generation, "work scheduling generation")?,
                staged_status,
                status_reference_id,
                serde_json::to_string(&demand.capabilities)?,
                serde_json::to_string(&demand.locks)?,
                &demand.locality,
                &demand.cost_class,
                serde_json::to_string(demand)?,
                &now,
            ],
        )?;
    }

    tx.execute(
        "DELETE FROM scheduler_yield_continuations WHERE agent_id = ?1",
        [agent_id],
    )?;
    for demand in snapshot.work.values() {
        if let WorkStatus::Yielded { continuation } = &demand.status {
            tx.execute(
                "INSERT INTO scheduler_yield_continuations (
                   agent_id, continuation_id, source_work_item_id, source_generation,
                   target_work_item_id, target_generation, payload_json, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    agent_id,
                    &continuation.continuation_id,
                    &continuation.source_work_item_id,
                    to_i64(continuation.source_generation, "yield source generation")?,
                    &continuation.target_work_item_id,
                    to_i64(continuation.target_generation, "yield target generation")?,
                    serde_json::to_string(continuation)?,
                    &now,
                ],
            )?;
        }
    }

    for (wait_id, wait) in &snapshot.waits {
        let owner = &wait
            .generations
            .get(&wait.current_generation)
            .ok_or_else(|| anyhow!("wait {wait_id} has no current generation"))?
            .owner;
        let (owner_kind, owner_id, owner_work_item_id) = scheduler_owner_columns(owner);
        tx.execute(
            "INSERT INTO scheduler_waits (
               agent_id,
               wait_id,
               owner_kind,
               owner_id,
               owner_work_item_id,
               current_generation,
               payload_json,
               updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(agent_id, wait_id) DO UPDATE SET
               owner_kind = excluded.owner_kind,
               owner_id = excluded.owner_id,
               owner_work_item_id = excluded.owner_work_item_id,
               current_generation = excluded.current_generation,
               payload_json = excluded.payload_json,
               updated_at = excluded.updated_at",
            params![
                agent_id,
                wait_id,
                owner_kind,
                owner_id,
                owner_work_item_id,
                to_i64(wait.current_generation, "wait generation")?,
                serde_json::to_string(wait)?,
                &now,
            ],
        )?;
        for (generation, record) in &wait.generations {
            let (trigger_id, trigger_generation) = match &record.trigger {
                Some(trigger) => (
                    Some(trigger.trigger_id.as_str()),
                    Some(to_i64(
                        trigger.trigger_generation,
                        "wait trigger generation",
                    )?),
                ),
                None => (None, None),
            };
            let staged_state = if record.state == WaitState::Consumed {
                enum_token(&WaitState::Triggered)?
            } else {
                enum_token(&record.state)?
            };
            let (owner_kind, owner_id, owner_work_item_id) = scheduler_owner_columns(&record.owner);
            tx.execute(
                "INSERT INTO scheduler_wait_generations (
                   agent_id,
                   wait_id,
                   generation,
                   owner_kind,
                   owner_id,
                   owner_work_item_id,
                   lifecycle_state,
                   trigger_id,
                   trigger_generation,
                   consuming_activation_id,
                   payload_json,
                   created_at,
                   updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, ?11, ?11)
                 ON CONFLICT(agent_id, wait_id, generation) DO UPDATE SET
                   owner_kind = excluded.owner_kind,
                   owner_id = excluded.owner_id,
                   owner_work_item_id = excluded.owner_work_item_id,
                   lifecycle_state = excluded.lifecycle_state,
                   trigger_id = excluded.trigger_id,
                   trigger_generation = excluded.trigger_generation,
                   consuming_activation_id = NULL,
                   payload_json = excluded.payload_json,
                   updated_at = excluded.updated_at",
                params![
                    agent_id,
                    wait_id,
                    to_i64(*generation, "wait generation")?,
                    owner_kind,
                    owner_id,
                    owner_work_item_id,
                    staged_state,
                    trigger_id,
                    trigger_generation,
                    serde_json::to_string(record)?,
                    &now,
                ],
            )?;
        }
    }

    for admission in snapshot.activation_admissions.values() {
        let owner = activation_owner(&admission.activation)?;
        let (owner_kind, owner_id, work_item_id) = scheduler_owner_columns(&owner);
        let authority = LegacyActivationAuthorityPayload {
            authority_id: &admission.authority_id,
            activation: &admission.activation,
            expected_scheduling_generation: admission.expected_scheduling_generation,
            expected_dispatch_revision: admission.expected_dispatch_revision,
            consumed_by: None,
        };
        tx.execute(
            "INSERT INTO scheduler_activation_authorities (
               agent_id,
               authority_id,
               activation_id,
               owner_kind,
               owner_id,
               work_item_id,
               expected_scheduling_generation,
               expected_dispatch_revision,
               consumed_by_activation_id,
               payload_json,
               created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10)
             ON CONFLICT(agent_id, authority_id) DO UPDATE SET
               activation_id = excluded.activation_id,
               owner_kind = excluded.owner_kind,
               owner_id = excluded.owner_id,
               work_item_id = excluded.work_item_id,
               expected_scheduling_generation = excluded.expected_scheduling_generation,
               expected_dispatch_revision = excluded.expected_dispatch_revision,
               consumed_by_activation_id = NULL,
               payload_json = excluded.payload_json",
            params![
                agent_id,
                &admission.authority_id,
                &admission.activation.id,
                owner_kind,
                owner_id,
                work_item_id,
                to_i64(
                    admission.expected_scheduling_generation,
                    "authority scheduling generation",
                )?,
                to_i64(
                    admission.expected_dispatch_revision,
                    "authority dispatch revision",
                )?,
                serde_json::to_string(&authority)?,
                &now,
            ],
        )?;
    }

    let ordered_activations = snapshot
        .activations
        .iter()
        .filter(|(_, activation)| activation.recovery_for.is_none())
        .chain(
            snapshot
                .activations
                .iter()
                .filter(|(_, activation)| activation.recovery_for.is_some()),
        );
    for (activation_id, activation) in ordered_activations {
        let admission = snapshot
            .activation_admissions
            .get(activation_id)
            .ok_or_else(|| anyhow!("activation {activation_id} has no canonical admission"))?;
        let (admission_kind, recovery_for, wait_id, wait_generation) =
            activation_admission_columns(admission)?;
        let (owner_kind, owner_id, work_item_id) = scheduler_owner_columns(&activation.owner);
        tx.execute(
            "INSERT INTO scheduler_activations (
               agent_id,
               activation_id,
               authority_id,
               owner_kind,
               owner_id,
               work_item_id,
               admitted_generation,
               admission_kind,
               recovery_for_activation_id,
               wait_id,
               wait_generation,
               lifecycle_state,
               idempotency_key,
               payload_json,
               created_at,
               updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15)
             ON CONFLICT(agent_id, activation_id) DO UPDATE SET
               authority_id = excluded.authority_id,
               owner_kind = excluded.owner_kind,
               owner_id = excluded.owner_id,
               work_item_id = excluded.work_item_id,
               admitted_generation = excluded.admitted_generation,
               admission_kind = excluded.admission_kind,
               recovery_for_activation_id = excluded.recovery_for_activation_id,
               wait_id = excluded.wait_id,
               wait_generation = excluded.wait_generation,
               lifecycle_state = excluded.lifecycle_state,
               idempotency_key = excluded.idempotency_key,
               payload_json = excluded.payload_json,
               updated_at = excluded.updated_at",
            params![
                agent_id,
                activation_id,
                &admission.authority_id,
                owner_kind,
                owner_id,
                work_item_id,
                to_i64(activation.admitted_generation, "admitted generation")?,
                admission_kind,
                recovery_for,
                wait_id,
                wait_generation,
                activation_state_token(&activation.state),
                &admission.activation.idempotency_key,
                serde_json::to_string(admission)?,
                &now,
            ],
        )?;
        if let Some((source_kind, source_identity)) = activation_source_columns(admission) {
            tx.execute(
                "INSERT INTO scheduler_activation_sources (
                   agent_id,
                   activation_id,
                   source_kind,
                   source_identity,
                   payload_json,
                   created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(agent_id, activation_id) DO UPDATE SET
                   source_kind = excluded.source_kind,
                   source_identity = excluded.source_identity,
                   payload_json = excluded.payload_json",
                params![
                    agent_id,
                    activation_id,
                    source_kind,
                    source_identity,
                    serde_json::to_string(&admission.activation.cause)?,
                    &now,
                ],
            )?;
        }
    }

    for admission in snapshot.activation_admissions.values() {
        let authority = LegacyActivationAuthorityPayload {
            authority_id: &admission.authority_id,
            activation: &admission.activation,
            expected_scheduling_generation: admission.expected_scheduling_generation,
            expected_dispatch_revision: admission.expected_dispatch_revision,
            consumed_by: Some(&admission.activation.id),
        };
        tx.execute(
            "UPDATE scheduler_activation_authorities
             SET consumed_by_activation_id = ?3, payload_json = ?4
             WHERE agent_id = ?1 AND authority_id = ?2",
            params![
                agent_id,
                &admission.authority_id,
                &admission.activation.id,
                serde_json::to_string(&authority)?,
            ],
        )?;
    }

    for (wait_id, wait) in &snapshot.waits {
        for (generation, record) in &wait.generations {
            tx.execute(
                "UPDATE scheduler_wait_generations
                 SET lifecycle_state = ?4,
                     consuming_activation_id = ?5,
                     payload_json = ?6
                 WHERE agent_id = ?1 AND wait_id = ?2 AND generation = ?3",
                params![
                    agent_id,
                    wait_id,
                    to_i64(*generation, "wait generation")?,
                    enum_token(&record.state)?,
                    record.consuming_activation_id.as_deref(),
                    serde_json::to_string(record)?,
                ],
            )?;
        }
    }

    persist_slot_tx(tx, agent_id, &snapshot.slot, &now)?;
    persist_dispatch_tx(
        tx,
        agent_id,
        &snapshot.dispatch,
        snapshot.dispatch_revision,
        &now,
    )?;
    let focus_revision = next_focus_revision_tx(tx, agent_id, snapshot.focus.as_deref())?;
    tx.execute(
        "INSERT INTO scheduler_agent_focus (
           agent_id, focused_work_item_id, focus_revision, updated_at
         ) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(agent_id) DO UPDATE SET
           focused_work_item_id = excluded.focused_work_item_id,
           focus_revision = excluded.focus_revision,
           updated_at = excluded.updated_at",
        params![
            agent_id,
            snapshot.focus.as_deref(),
            to_i64(focus_revision, "focus revision")?,
            &now,
        ],
    )?;

    for (work_item_id, demand) in &snapshot.work {
        if matches!(demand.status, WorkStatus::Terminal) {
            tx.execute(
                "UPDATE scheduler_work_demands
                 SET status = 'terminal',
                     status_reference_id = NULL,
                     payload_json = ?3,
                     updated_at = ?4
                 WHERE agent_id = ?1 AND work_item_id = ?2",
                params![agent_id, work_item_id, serde_json::to_string(demand)?, &now,],
            )?;
        }
    }

    for (settlement_id, settlement) in &snapshot.settlements {
        tx.execute(
            "INSERT INTO scheduler_activation_settlements (
               agent_id, settlement_id, activation_id, payload_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(agent_id, settlement_id) DO UPDATE SET
               activation_id = excluded.activation_id,
               payload_json = excluded.payload_json",
            params![
                agent_id,
                settlement_id,
                &settlement.activation_id,
                serde_json::to_string(settlement)?,
                &settlement.created_at,
            ],
        )?;
    }
    for (missing_id, missing) in &snapshot.missing_settlements {
        tx.execute(
            "INSERT INTO scheduler_missing_settlements (
               agent_id, missing_settlement_id, activation_id, payload_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(agent_id, missing_settlement_id) DO UPDATE SET
               activation_id = excluded.activation_id,
               payload_json = excluded.payload_json",
            params![
                agent_id,
                missing_id,
                &missing.activation_id,
                serde_json::to_string(missing)?,
                &missing.created_at,
            ],
        )?;
    }
    for (admission_id, admission) in &snapshot.continuation_admissions {
        tx.execute(
            "INSERT INTO scheduler_continuation_admissions (
               agent_id,
               admission_id,
               settlement_id,
               completed_work_item_id,
               caller_work_item_id,
               expected_caller_generation,
               admitted_caller_generation,
               payload_json,
               created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(agent_id, admission_id) DO UPDATE SET
               settlement_id = excluded.settlement_id,
               completed_work_item_id = excluded.completed_work_item_id,
               caller_work_item_id = excluded.caller_work_item_id,
               expected_caller_generation = excluded.expected_caller_generation,
               admitted_caller_generation = excluded.admitted_caller_generation,
               payload_json = excluded.payload_json",
            params![
                agent_id,
                admission_id,
                &admission.settlement_id,
                &admission.completed_work_item_id,
                &admission.caller_work_item_id,
                to_i64(
                    admission.expected_caller_generation,
                    "expected caller generation",
                )?,
                to_i64(
                    admission.admitted_caller_generation,
                    "admitted caller generation",
                )?,
                serde_json::to_string(admission)?,
                &now,
            ],
        )?;
    }
    for (attachment_id, attachment) in &snapshot.activation_inputs {
        let (owner_kind, owner_id, _) = scheduler_owner_columns(&attachment.owner);
        tx.execute(
            "INSERT INTO scheduler_activation_inputs (
               agent_id,
               attachment_id,
               activation_id,
               owner_kind,
               owner_id,
               expected_admitted_generation,
               expected_dispatch_revision,
               message_id,
               turn_id,
               boundary,
               round,
               payload_json,
               created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(agent_id, attachment_id) DO UPDATE SET
               activation_id = excluded.activation_id,
               owner_kind = excluded.owner_kind,
               owner_id = excluded.owner_id,
               expected_admitted_generation = excluded.expected_admitted_generation,
               expected_dispatch_revision = excluded.expected_dispatch_revision,
               message_id = excluded.message_id,
               turn_id = excluded.turn_id,
               boundary = excluded.boundary,
               round = excluded.round,
               payload_json = excluded.payload_json",
            params![
                agent_id,
                attachment_id,
                &attachment.activation_id,
                owner_kind,
                owner_id,
                to_i64(
                    attachment.expected_admitted_generation,
                    "activation input admitted generation",
                )?,
                to_i64(
                    attachment.expected_dispatch_revision,
                    "activation input dispatch revision",
                )?,
                &attachment.message_id,
                &attachment.turn_id,
                &attachment.boundary,
                to_i64(attachment.round, "activation input round")?,
                serde_json::to_string(attachment)?,
                &attachment.created_at,
            ],
        )?;
    }
    Ok(())
}

fn load_snapshot_connection(connection: &Connection, agent_id: &str) -> Result<Snapshot> {
    load_snapshot_connection_with_hook(connection, agent_id, || Ok(()))
}

fn load_snapshot_connection_with_hook(
    connection: &Connection,
    agent_id: &str,
    after_first_read: impl FnOnce() -> Result<()>,
) -> Result<Snapshot> {
    let slot = load_slot(connection, agent_id)?;
    after_first_read()?;
    let (dispatch, dispatch_revision) = load_dispatch(connection, agent_id)?;
    let focus = connection
        .query_row(
            "SELECT focused_work_item_id FROM scheduler_agent_focus WHERE agent_id = ?1",
            [agent_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("scheduler protocol partition {agent_id} is missing focus row"))?;

    let work = load_payload_map::<WorkDemand>(
        connection,
        "SELECT work_item_id, payload_json FROM scheduler_work_demands WHERE agent_id = ?1",
        agent_id,
    )?;
    let waits = load_waits(connection, agent_id)?;
    let activation_admissions = load_payload_map::<AdmitActivationCommand>(
        connection,
        "SELECT activation_id, payload_json FROM scheduler_activations WHERE agent_id = ?1",
        agent_id,
    )?;
    let activations = load_activations(connection, agent_id)?;
    let settlements = load_payload_map(
        connection,
        "SELECT settlement_id, payload_json FROM scheduler_activation_settlements WHERE agent_id = ?1",
        agent_id,
    )?;
    let missing_settlements = load_payload_map::<MissingSettlementRecord>(
        connection,
        "SELECT missing_settlement_id, payload_json FROM scheduler_missing_settlements WHERE agent_id = ?1",
        agent_id,
    )?;
    let continuation_admissions = load_payload_map::<ContinuationAdmissionRecord>(
        connection,
        "SELECT admission_id, payload_json FROM scheduler_continuation_admissions WHERE agent_id = ?1",
        agent_id,
    )?;
    let activation_inputs = load_payload_map::<ActivationInputAttachment>(
        connection,
        "SELECT attachment_id, payload_json FROM scheduler_activation_inputs WHERE agent_id = ?1",
        agent_id,
    )?;
    let admitted_generations = activation_admissions
        .values()
        .map(persisted_admission_fence)
        .collect::<Result<BTreeSet<_>>>()?;
    let snapshot = Snapshot {
        slot,
        dispatch,
        dispatch_revision,
        focus,
        work,
        waits,
        activations,
        activation_admissions,
        settlements,
        missing_settlements,
        admitted_generations,
        continuation_admissions,
        activation_inputs,
    };
    validate_agent_partition(agent_id, &snapshot)?;
    scheduler_protocol::assert_invariants(&snapshot)
        .map_err(|error| anyhow!("invalid persisted scheduler protocol snapshot: {error}"))?;
    Ok(snapshot)
}

fn load_snapshot_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<Snapshot> {
    load_snapshot_connection(tx, agent_id)
}

fn to_i64(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value).with_context(|| format!("{field} exceeds SQLite INTEGER range"))
}

fn to_u64(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).with_context(|| format!("{field} is negative"))
}

fn work_status_columns(status: &WorkStatus) -> (&'static str, Option<&str>) {
    match status {
        WorkStatus::Runnable => ("runnable", None),
        WorkStatus::Waiting { wait_id } => ("waiting", Some(wait_id)),
        WorkStatus::Yielded { continuation } => {
            ("paused", Some(continuation.continuation_id.as_str()))
        }
        WorkStatus::NeedsSettlement { activation_id } => ("needs_settlement", Some(activation_id)),
        WorkStatus::Paused { hold_id } => ("paused", Some(hold_id)),
        WorkStatus::Terminal => ("terminal", None),
    }
}

fn activation_owner(activation: &scheduler_protocol::AgentActivation) -> Result<SchedulerOwner> {
    match &activation.binding {
        scheduler_protocol::ActivationBinding::WorkItem { work_item_id } => {
            Ok(SchedulerOwner::WorkItem {
                work_item_id: work_item_id.clone(),
            })
        }
        scheduler_protocol::ActivationBinding::WaitOwner { owner, .. } => Ok(owner.clone()),
        scheduler_protocol::ActivationBinding::Lifecycle { agent_id } => {
            Ok(SchedulerOwner::AgentLifecycle {
                agent_id: agent_id.clone(),
            })
        }
        _ => bail!(
            "activation {} has no scheduler owner binding",
            activation.id
        ),
    }
}

fn scheduler_owner_columns(owner: &SchedulerOwner) -> (&'static str, &str, Option<&str>) {
    match owner {
        SchedulerOwner::WorkItem { work_item_id } => {
            ("work_item", work_item_id, Some(work_item_id))
        }
        SchedulerOwner::AgentLifecycle { agent_id } => ("agent_lifecycle", agent_id, None),
    }
}

fn scheduler_owner_fence_prefix(owner: &SchedulerOwner) -> String {
    match owner {
        SchedulerOwner::WorkItem { work_item_id } => format!("work:{work_item_id}"),
        SchedulerOwner::AgentLifecycle { agent_id } => format!("lifecycle:{agent_id}"),
    }
}

fn scheduler_owner_from_columns(
    owner_kind: &str,
    owner_id: String,
    agent_id: &str,
) -> Result<SchedulerOwner> {
    match owner_kind {
        "work_item" if !owner_id.is_empty() => Ok(SchedulerOwner::WorkItem {
            work_item_id: owner_id,
        }),
        "agent_lifecycle" if owner_id == agent_id => {
            Ok(SchedulerOwner::AgentLifecycle { agent_id: owner_id })
        }
        _ => bail!("invalid scheduler owner {owner_kind}:{owner_id} for agent {agent_id}"),
    }
}

fn activation_admission_columns(
    admission: &AdmitActivationCommand,
) -> Result<(&'static str, Option<&str>, Option<&str>, Option<i64>)> {
    match &admission.activation.cause {
        ActivationCause::WorkItemRunnable { .. } => Ok(("scheduling", None, None, None)),
        ActivationCause::TaskRejoin { resume, .. }
        | ActivationCause::OperatorInput { resume, .. } => match resume {
            Some(resume) => Ok((
                "wait_resume",
                None,
                Some(&resume.wait_id),
                Some(to_i64(
                    resume.wait_generation,
                    "embedded wait resume generation",
                )?),
            )),
            None => Ok(("scheduling", None, None, None)),
        },
        ActivationCause::WaitResume {
            wait_id,
            wait_generation,
            ..
        } => Ok((
            "wait_resume",
            None,
            Some(wait_id),
            Some(to_i64(*wait_generation, "wait resume generation")?),
        )),
        ActivationCause::SettlementRecovery { activation_id } => {
            Ok(("settlement_recovery", Some(activation_id), None, None))
        }
        ActivationCause::LifecycleExternalNudge { .. } => {
            Ok(("lifecycle_external_nudge", None, None, None))
        }
        _ => bail!(
            "activation {} has unsupported persisted admission cause",
            admission.activation.id
        ),
    }
}

fn persisted_admission_fence(admission: &AdmitActivationCommand) -> Result<String> {
    let activation = &admission.activation;
    let work_item_id = match (&activation.cause, &activation.binding) {
        (
            ActivationCause::WorkItemRunnable { work_item_id, .. },
            scheduler_protocol::ActivationBinding::WorkItem {
                work_item_id: bound_work_item_id,
            },
        ) if work_item_id == bound_work_item_id => work_item_id,
        (
            ActivationCause::TaskRejoin { task_id, .. },
            scheduler_protocol::ActivationBinding::WorkItem { .. },
        ) => return Ok(format!("task:{task_id}")),
        (
            ActivationCause::OperatorInput { message_id, .. },
            scheduler_protocol::ActivationBinding::WorkItem { .. },
        ) => return Ok(format!("operator_message:{message_id}")),
        (
            ActivationCause::WaitResume { wait_id, .. },
            scheduler_protocol::ActivationBinding::WaitOwner {
                wait_id: bound_wait_id,
                owner,
            },
        ) if wait_id == bound_wait_id => {
            return Ok(format!(
                "{}:{}",
                scheduler_owner_fence_prefix(owner),
                admission.expected_scheduling_generation
            ));
        }
        (
            ActivationCause::WaitResume { wait_id: _, .. },
            scheduler_protocol::ActivationBinding::Lifecycle { agent_id },
        ) => {
            return Ok(format!(
                "lifecycle:{agent_id}:{}",
                admission.expected_scheduling_generation
            ));
        }
        (
            ActivationCause::LifecycleExternalNudge { message_id },
            scheduler_protocol::ActivationBinding::Lifecycle { .. },
        ) => return Ok(format!("lifecycle_message:{message_id}")),
        (
            ActivationCause::SettlementRecovery { activation_id },
            scheduler_protocol::ActivationBinding::WorkItem { work_item_id },
        ) => {
            return Ok(format!(
                "work:{work_item_id}:{}:recovery:{activation_id}",
                admission.expected_scheduling_generation
            ));
        }
        _ => bail!(
            "activation {} has no canonical persisted admission fence",
            activation.id
        ),
    };
    Ok(format!(
        "work:{work_item_id}:{}",
        admission.expected_scheduling_generation
    ))
}

fn activation_source_columns(admission: &AdmitActivationCommand) -> Option<(&'static str, &str)> {
    match &admission.activation.cause {
        ActivationCause::TaskRejoin { task_id, .. } => Some(("task_rejoin", task_id)),
        ActivationCause::OperatorInput { message_id, .. } => Some(("operator_input", message_id)),
        _ => None,
    }
}

fn activation_state_token(state: &ActivationState) -> &'static str {
    match state {
        ActivationState::Running => "running",
        ActivationState::Settled => "settled",
        ActivationState::SettlementMissing => "settlement_missing",
    }
}

fn persist_slot_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    slot: &ActivationSlot,
    now: &str,
) -> Result<()> {
    let (
        slot_kind,
        activation_id,
        owner_kind,
        owner_id,
        work_item_id,
        admitted_generation,
        recovery_for,
    ) = match slot {
        ActivationSlot::Idle => ("idle", None, None, None, None, None, None),
        ActivationSlot::Running {
            activation_id,
            owner,
            admitted_generation,
            recovery_for,
        } => {
            let (owner_kind, owner_id, work_item_id) = scheduler_owner_columns(owner);
            (
                "running",
                Some(activation_id.as_str()),
                Some(owner_kind),
                Some(owner_id),
                work_item_id,
                Some(to_i64(*admitted_generation, "slot admitted generation")?),
                recovery_for.as_deref(),
            )
        }
    };
    tx.execute(
        "INSERT INTO scheduler_agent_slots (
           agent_id,
           slot_kind,
           activation_id,
           owner_kind,
           owner_id,
           work_item_id,
           admitted_generation,
           recovery_for_activation_id,
           updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(agent_id) DO UPDATE SET
           slot_kind = excluded.slot_kind,
           activation_id = excluded.activation_id,
           owner_kind = excluded.owner_kind,
           owner_id = excluded.owner_id,
           work_item_id = excluded.work_item_id,
           admitted_generation = excluded.admitted_generation,
           recovery_for_activation_id = excluded.recovery_for_activation_id,
           updated_at = excluded.updated_at",
        params![
            agent_id,
            slot_kind,
            activation_id,
            owner_kind,
            owner_id,
            work_item_id,
            admitted_generation,
            recovery_for,
            now,
        ],
    )?;
    Ok(())
}

fn persist_dispatch_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    dispatch: &AgentDispatchState,
    dispatch_revision: u64,
    now: &str,
) -> Result<()> {
    let (dispatch_kind, wait_id, wait_generation) = match dispatch {
        AgentDispatchState::Open => ("open", None, None),
        AgentDispatchState::Awaiting { wait } => (
            "awaiting",
            Some(wait.id.as_str()),
            Some(to_i64(wait.generation, "dispatch wait generation")?),
        ),
    };
    tx.execute(
        "INSERT INTO scheduler_agent_dispatch (
           agent_id,
           dispatch_kind,
           wait_id,
           wait_generation,
           dispatch_revision,
           updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(agent_id) DO UPDATE SET
           dispatch_kind = excluded.dispatch_kind,
           wait_id = excluded.wait_id,
           wait_generation = excluded.wait_generation,
           dispatch_revision = excluded.dispatch_revision,
           updated_at = excluded.updated_at",
        params![
            agent_id,
            dispatch_kind,
            wait_id,
            wait_generation,
            to_i64(dispatch_revision, "dispatch revision")?,
            now,
        ],
    )?;
    Ok(())
}

fn next_focus_revision_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    focused_work_item_id: Option<&str>,
) -> Result<u64> {
    let existing = tx
        .query_row(
            "SELECT focused_work_item_id, focus_revision
             FROM scheduler_agent_focus
             WHERE agent_id = ?1",
            [agent_id],
            |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    match existing {
        None => Ok(0),
        Some((existing_focus, revision)) if existing_focus.as_deref() == focused_work_item_id => {
            to_u64(revision, "focus revision")
        }
        Some((_, revision)) => to_u64(revision, "focus revision")?
            .checked_add(1)
            .ok_or_else(|| anyhow!("focus revision overflow")),
    }
}

fn load_slot(connection: &Connection, agent_id: &str) -> Result<ActivationSlot> {
    let (slot_kind, activation_id, owner_kind, owner_id, admitted_generation, recovery_for) =
        connection
            .query_row(
                "SELECT
               slot_kind,
               activation_id,
               owner_kind,
               owner_id,
               admitted_generation,
               recovery_for_activation_id
             FROM scheduler_agent_slots
             WHERE agent_id = ?1",
                [agent_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                anyhow!("scheduler protocol partition {agent_id} is missing slot row")
            })?;
    match (
        slot_kind.as_str(),
        activation_id,
        owner_kind,
        owner_id,
        admitted_generation,
        recovery_for,
    ) {
        ("idle", None, None, None, None, None) => Ok(ActivationSlot::Idle),
        (
            "running",
            Some(activation_id),
            Some(owner_kind),
            Some(owner_id),
            Some(admitted_generation),
            recovery_for,
        ) => Ok(ActivationSlot::Running {
            activation_id,
            owner: scheduler_owner_from_columns(&owner_kind, owner_id, agent_id)?,
            admitted_generation: to_u64(admitted_generation, "slot admitted generation")?,
            recovery_for,
        }),
        _ => bail!("scheduler protocol slot row for agent {agent_id} is invalid"),
    }
}

fn load_dispatch(connection: &Connection, agent_id: &str) -> Result<(AgentDispatchState, u64)> {
    let (dispatch_kind, wait_id, wait_generation, dispatch_revision) = connection
        .query_row(
            "SELECT dispatch_kind, wait_id, wait_generation, dispatch_revision
             FROM scheduler_agent_dispatch
             WHERE agent_id = ?1",
            [agent_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            anyhow!("scheduler protocol partition {agent_id} is missing dispatch row")
        })?;
    let dispatch = match (dispatch_kind.as_str(), wait_id, wait_generation) {
        ("open", None, None) => AgentDispatchState::Open,
        ("awaiting", Some(id), Some(generation)) => AgentDispatchState::Awaiting {
            wait: WaitIdentity {
                id,
                generation: to_u64(generation, "dispatch wait generation")?,
            },
        },
        _ => bail!("scheduler protocol dispatch row for agent {agent_id} is invalid"),
    };
    Ok((dispatch, to_u64(dispatch_revision, "dispatch revision")?))
}

fn load_payload_map<T>(
    connection: &Connection,
    sql: &str,
    agent_id: &str,
) -> Result<BTreeMap<String, T>>
where
    T: for<'de> Deserialize<'de>,
{
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([agent_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut records = BTreeMap::new();
    for row in rows {
        let (identity, payload_json) = row?;
        let record = serde_json::from_str(&payload_json)
            .with_context(|| format!("invalid scheduler protocol payload for {identity}"))?;
        if records.insert(identity.clone(), record).is_some() {
            bail!("duplicate scheduler protocol identity {identity}");
        }
    }
    Ok(records)
}

fn load_waits(connection: &Connection, agent_id: &str) -> Result<BTreeMap<String, WaitRecord>> {
    let mut waits = BTreeMap::new();
    let mut statement = connection.prepare(
        "SELECT wait_id, current_generation
         FROM scheduler_waits
         WHERE agent_id = ?1
         ORDER BY wait_id",
    )?;
    let rows = statement.query_map([agent_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (wait_id, current_generation) = row?;
        waits.insert(
            wait_id,
            WaitRecord {
                current_generation: to_u64(current_generation, "wait current generation")?,
                generations: BTreeMap::new(),
            },
        );
    }
    let mut statement = connection.prepare(
        "SELECT
           wait_id,
           generation,
           owner_kind,
           owner_id,
           lifecycle_state,
           trigger_id,
           trigger_generation,
           consuming_activation_id
         FROM scheduler_wait_generations
         WHERE agent_id = ?1
         ORDER BY wait_id, generation",
    )?;
    let rows = statement.query_map([agent_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<i64>>(6)?,
            row.get::<_, Option<String>>(7)?,
        ))
    })?;
    for row in rows {
        let (
            wait_id,
            generation,
            owner_kind,
            owner_id,
            lifecycle_state,
            trigger_id,
            trigger_generation,
            consuming_activation_id,
        ) = row?;
        let generation = to_u64(generation, "wait generation")?;
        let state = match lifecycle_state.as_str() {
            "active" => WaitState::Active,
            "triggered" => WaitState::Triggered,
            "consumed" => WaitState::Consumed,
            "resolved" => WaitState::Resolved,
            _ => bail!("wait {wait_id} generation {generation} has invalid lifecycle state"),
        };
        let trigger = match (trigger_id, trigger_generation) {
            (None, None) => None,
            (Some(trigger_id), Some(trigger_generation)) => Some(WaitTrigger {
                trigger_id,
                trigger_generation: to_u64(trigger_generation, "wait trigger generation")?,
            }),
            _ => bail!("wait {wait_id} generation {generation} has partial trigger identity"),
        };
        let record = WaitGenerationRecord {
            owner: scheduler_owner_from_columns(&owner_kind, owner_id, agent_id)?,
            state,
            trigger,
            consuming_activation_id,
        };
        waits
            .get_mut(&wait_id)
            .ok_or_else(|| anyhow!("wait generation references missing wait {wait_id}"))?
            .generations
            .insert(generation, record);
    }
    Ok(waits)
}

fn load_activations(
    connection: &Connection,
    agent_id: &str,
) -> Result<BTreeMap<String, ActivationRecord>> {
    let mut statement = connection.prepare(
        "SELECT
           activation_id,
           owner_kind,
           owner_id,
           admitted_generation,
           lifecycle_state,
           recovery_for_activation_id
         FROM scheduler_activations
         WHERE agent_id = ?1
         ORDER BY activation_id",
    )?;
    let rows = statement.query_map([agent_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;
    let mut activations = BTreeMap::new();
    for row in rows {
        let (
            activation_id,
            owner_kind,
            owner_id,
            admitted_generation,
            lifecycle_state,
            recovery_for,
        ) = row?;
        let state = match lifecycle_state.as_str() {
            "admitted" | "running" => ActivationState::Running,
            "settled" | "interrupted" | "cancelled" => ActivationState::Settled,
            "settlement_missing" => ActivationState::SettlementMissing,
            _ => bail!("activation {activation_id} has invalid lifecycle state"),
        };
        activations.insert(
            activation_id,
            ActivationRecord {
                owner: scheduler_owner_from_columns(&owner_kind, owner_id, agent_id)?,
                admitted_generation: to_u64(admitted_generation, "admitted generation")?,
                state,
                recovery_for,
            },
        );
    }
    Ok(activations)
}
