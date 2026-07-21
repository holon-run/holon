use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error as StdError,
    fmt,
};

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::domain::scheduler_protocol::{
    self, ActivationAdmissionAuthority, ActivationCause, ActivationRecord, ActivationSlot,
    ActivationState, AdmitActivationCommand, AgentDispatchState, ContinuationAdmissionRecord,
    Decision, MissingSettlementRecord, ProtocolCommand, ProtocolConflict, ProtocolConflictKind,
    ProtocolMode, RollbackAction, RollbackTrigger, RolloutCommand, RolloutManifest,
    RolloutPreflightRecord, RolloutPreflightState, RolloutState, ScenarioAuthority,
    ScenarioHardBlockerRecord, ScenarioMode, Snapshot, WaitGenerationRecord, WaitIdentity,
    WaitRecord, WorkDemand, WorkStatus,
};
use crate::domain::scheduler_semantic::{
    resolve_semantic_proposal, validate_semantic_decision_input, validate_semantic_provider_config,
    SemanticDecisionInput, SemanticProposalProviderConfig, SemanticProposalResolution,
    SemanticProposalResponse, SemanticValidationPolicy, SEMANTIC_CONTRACT_REVISION,
    SEMANTIC_OPERATOR_BINDING_SCENARIO,
};

use super::{inject_fault, RuntimeTransitionRepository, TransitionFaultPoint};

const CANONICAL_COMMAND_SCHEMA_VERSION: i64 = 1;
const SHADOW_COMPARISON_SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SchedulerShadowComparisonCommand {
    pub scenario_class: String,
    pub comparison_identity: String,
    pub boundary: String,
    pub input_identity: String,
    pub legacy_observation: serde_json::Value,
    pub shadow_candidate: serde_json::Value,
    pub matched: bool,
    #[serde(default)]
    pub divergence_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SchedulerSemanticShadowCommand {
    pub input: SemanticDecisionInput,
    pub provider: SemanticProposalProviderConfig,
    pub response: SemanticProposalResponse,
    pub policy: SemanticValidationPolicy,
}

pub(super) struct PreparedShadowComparison {
    command: SchedulerShadowComparisonCommand,
    payload_hash: String,
    authority_mode: ScenarioMode,
    already_recorded: bool,
}

pub(super) struct PreparedSemanticShadowDecision {
    command: SchedulerSemanticShadowCommand,
    resolution: SemanticProposalResolution,
    payload_hash: String,
    authority_mode: ScenarioMode,
    already_recorded: bool,
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
pub(crate) struct SchedulerRolloutTransitionCommit {
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

pub(super) fn validate_shadow_comparison_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    command: Option<&SchedulerShadowComparisonCommand>,
) -> Result<Option<PreparedShadowComparison>> {
    let Some(command) = command else {
        return Ok(None);
    };
    if agent_id.is_empty()
        || command.scenario_class.is_empty()
        || command.comparison_identity.is_empty()
        || command.boundary.is_empty()
        || command.input_identity.is_empty()
    {
        bail!("scheduler shadow comparison requires non-empty canonical identities");
    }
    if command.matched != command.divergence_code.is_none() {
        bail!("scheduler shadow comparison divergence code disagrees with outcome");
    }

    let authority_mode = effective_scenario_mode_tx(tx, &command.scenario_class)?;
    match authority_mode {
        ScenarioMode::Off => return Ok(None),
        ScenarioMode::Authoritative => {
            if !command.matched {
                bail!(
                    "scheduler scenario {} rejected divergent canonical evidence",
                    command.scenario_class
                );
            }
        }
        ScenarioMode::Shadow => {}
    }

    let payload_hash = canonical_shadow_comparison_hash(command)?;
    let existing_payload_hash = tx
        .query_row(
            "SELECT payload_hash
             FROM scheduler_shadow_comparisons
             WHERE agent_id = ?1
               AND scenario_class = ?2
               AND comparison_identity = ?3",
            params![
                agent_id,
                command.scenario_class,
                command.comparison_identity
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(existing_payload_hash) = existing_payload_hash.as_ref() {
        if existing_payload_hash != &payload_hash {
            bail!(
                "scheduler shadow comparison identity conflict for agent {}, scenario {}, comparison {}",
                agent_id,
                command.scenario_class,
                command.comparison_identity
            );
        }
    }

    Ok(Some(PreparedShadowComparison {
        command: command.clone(),
        payload_hash,
        authority_mode,
        already_recorded: existing_payload_hash.is_some(),
    }))
}

pub(super) fn validate_semantic_shadow_decision_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    command: Option<&SchedulerSemanticShadowCommand>,
) -> Result<Option<PreparedSemanticShadowDecision>> {
    let Some(command) = command else {
        return Ok(None);
    };
    if command.input.target_agent_id != agent_id {
        bail!("semantic shadow decision target agent does not match queue partition");
    }
    validate_semantic_decision_input(&command.input)
        .map_err(|error| anyhow!("invalid semantic decision input: {error:?}"))?;
    validate_semantic_provider_config(&command.provider)
        .map_err(|error| anyhow!("invalid semantic provider config: {error:?}"))?;
    command
        .policy
        .validate()
        .map_err(|error| anyhow!("invalid semantic validation policy: {error:?}"))?;

    let authority_mode = effective_scenario_mode_tx(tx, SEMANTIC_OPERATOR_BINDING_SCENARIO)?;
    match authority_mode {
        ScenarioMode::Off => return Ok(None),
        ScenarioMode::Authoritative => {
            bail!(
                "scheduler scenario {} is authoritative, but semantic production authority is not wired",
                SEMANTIC_OPERATOR_BINDING_SCENARIO
            );
        }
        ScenarioMode::Shadow => {}
    }

    let payload_hash = canonical_semantic_shadow_hash(command)?;
    let existing_payload_hash = tx
        .query_row(
            "SELECT payload_hash
             FROM scheduler_semantic_shadow_decisions
             WHERE agent_id = ?1 AND source_id = ?2",
            params![agent_id, command.input.provenance.source_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(existing_payload_hash) = existing_payload_hash.as_ref() {
        if existing_payload_hash != &payload_hash {
            bail!(
                "semantic decision source replay conflict for agent {}, source {}",
                agent_id,
                command.input.provenance.source_id
            );
        }
    }

    let resolution = resolve_semantic_proposal(
        &command.input,
        command.provider.clone(),
        command.response.clone(),
        command.policy,
    );
    Ok(Some(PreparedSemanticShadowDecision {
        command: command.clone(),
        resolution,
        payload_hash,
        authority_mode,
        already_recorded: existing_payload_hash.is_some(),
    }))
}

pub(super) fn persist_shadow_comparison_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    prepared: Option<PreparedShadowComparison>,
) -> Result<()> {
    let Some(prepared) = prepared else {
        return Ok(());
    };
    if prepared.already_recorded {
        return Ok(());
    }
    let command = prepared.command;
    tx.execute(
        "INSERT INTO scheduler_shadow_comparisons (
           agent_id, scenario_class, comparison_identity,
           canonical_schema_version, payload_hash, boundary, input_identity,
           authority_mode, legacy_observation_json, shadow_candidate_json,
           comparison_outcome, divergence_code, created_at
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13
         )",
        params![
            agent_id,
            command.scenario_class,
            command.comparison_identity,
            SHADOW_COMPARISON_SCHEMA_VERSION,
            prepared.payload_hash,
            command.boundary,
            command.input_identity,
            scenario_mode_token(prepared.authority_mode),
            serde_json::to_string(&command.legacy_observation)?,
            serde_json::to_string(&command.shadow_candidate)?,
            if command.matched {
                "matched"
            } else {
                "diverged"
            },
            command.divergence_code,
            Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(())
}

pub(super) fn persist_semantic_shadow_decision_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    prepared: Option<PreparedSemanticShadowDecision>,
) -> Result<()> {
    let Some(prepared) = prepared else {
        return Ok(());
    };
    if prepared.already_recorded {
        return Ok(());
    }
    let command = prepared.command;
    tx.execute(
        "INSERT INTO scheduler_semantic_shadow_decisions (
           agent_id, source_id, contract_revision, payload_hash, authority_mode,
           input_json, provider_json, response_json, policy_json, resolution_json,
           created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            agent_id,
            command.input.provenance.source_id,
            SEMANTIC_CONTRACT_REVISION,
            prepared.payload_hash,
            scenario_mode_token(prepared.authority_mode),
            serde_json::to_string(&command.input)?,
            serde_json::to_string(&command.provider)?,
            serde_json::to_string(&command.response)?,
            serde_json::to_string(&command.policy)?,
            serde_json::to_string(&prepared.resolution)?,
            Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn effective_scenario_mode_tx(tx: &Transaction<'_>, scenario_class: &str) -> Result<ScenarioMode> {
    let protocol_mode = tx.query_row(
        "SELECT protocol_mode
         FROM scheduler_protocol_config
         WHERE config_id = 1",
        [],
        |row| row.get::<_, String>(0),
    )?;
    let scenario_mode = tx
        .query_row(
            "SELECT mode
             FROM scheduler_scenario_authorities
             WHERE scenario_class = ?1",
            [scenario_class],
            |row| row.get::<_, String>(0),
        )
        .optional()?;

    match (protocol_mode.as_str(), scenario_mode.as_deref()) {
        ("legacy", _) | (_, None | Some("off")) => Ok(ScenarioMode::Off),
        ("shadow", Some("shadow")) | ("authoritative", Some("shadow")) => {
            Ok(ScenarioMode::Shadow)
        }
        ("authoritative", Some("authoritative")) => Ok(ScenarioMode::Authoritative),
        ("shadow", Some("authoritative")) => {
            bail!("scheduler scenario authority exceeds the protocol mode ceiling")
        }
        (mode, scenario) => bail!(
            "invalid scheduler rollout authority state: protocol_mode={mode}, scenario_mode={scenario:?}"
        ),
    }
}

fn canonical_shadow_comparison_hash(command: &SchedulerShadowComparisonCommand) -> Result<String> {
    let canonical = serde_json::to_vec(&serde_json::json!({
        "schema_version": SHADOW_COMPARISON_SCHEMA_VERSION,
        "command": command,
    }))?;
    Ok(format!("sha256:{:x}", Sha256::digest(canonical)))
}

fn canonical_semantic_shadow_hash(command: &SchedulerSemanticShadowCommand) -> Result<String> {
    let canonical = serde_json::to_vec(&serde_json::json!({
        "contract_revision": SEMANTIC_CONTRACT_REVISION,
        "command": command,
    }))?;
    Ok(format!("sha256:{:x}", Sha256::digest(canonical)))
}

fn scenario_mode_token(mode: ScenarioMode) -> &'static str {
    match mode {
        ScenarioMode::Off => "off",
        ScenarioMode::Shadow => "shadow",
        ScenarioMode::Authoritative => "authoritative",
    }
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
            let mut initialized = snapshot.clone();
            initialized.rollout = load_rollout(tx)?;
            scheduler_protocol::assert_invariants(&initialized)
                .map_err(|error| anyhow!("invalid scheduler protocol snapshot: {error}"))?;
            persist_agent_snapshot_tx(tx, agent_id, &initialized)?;
            Ok(())
        })
    }

    pub(crate) fn load_scheduler_protocol_snapshot(&self, agent_id: &str) -> Result<Snapshot> {
        self.load_scheduler_protocol_snapshot_with_hook(agent_id, || Ok(()))
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

    #[cfg(test)]
    pub(crate) fn load_scheduler_protocol_snapshot_paused_after_first_read(
        &self,
        agent_id: &str,
        after_first_read: impl FnOnce() -> Result<()>,
    ) -> Result<Snapshot> {
        self.load_scheduler_protocol_snapshot_with_hook(agent_id, after_first_read)
    }

    pub(crate) fn commit_scheduler_protocol_command(
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

    pub(crate) fn commit_scheduler_rollout_command(
        &self,
        command_identity: &str,
        command: &RolloutCommand,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<SchedulerRolloutTransitionCommit> {
        if command_identity.is_empty() {
            bail!("scheduler rollout command requires a non-empty identity");
        }
        let command_kind = rollout_command_kind(command);
        let payload_hash = canonical_rollout_command_hash(command_kind, command)?;

        let outcome = self.db.transaction(|tx| {
            if let Some(stored) =
                stored_rollout_command_result_tx(tx, command_kind, command_identity)?
            {
                if stored.payload_hash != payload_hash {
                    let conflict = insert_command_identity_conflict_attempt_tx(
                        tx,
                        "global_rollout",
                        "global",
                        command_kind,
                        command_identity,
                        &stored.payload_hash,
                        &payload_hash,
                    )?;
                    return Ok(CommandTransactionOutcome::Conflict(conflict));
                }
                return Ok(CommandTransactionOutcome::Commit(
                    SchedulerRolloutTransitionCommit {
                        applied: false,
                        replayed: true,
                        result: stored.result,
                    },
                ));
            }

            let rollout = load_rollout(tx)?;
            let snapshot = rollout_snapshot(rollout.clone());
            let outcome = scheduler_protocol::reduce_rollout_command(&snapshot, command);
            scheduler_protocol::assert_invariants(&outcome.outcome.snapshot).map_err(|error| {
                anyhow!("scheduler rollout reducer produced invalid state: {error}")
            })?;
            inject_fault(fault, TransitionFaultPoint::AfterValidation)?;

            if outcome.outcome.decision != Decision::Rejected {
                persist_rollout_tx(tx, &rollout, &outcome.outcome.snapshot.rollout)?;
            }
            inject_fault(fault, TransitionFaultPoint::AfterCanonicalWrites)?;

            let decision = outcome.outcome.decision.clone();
            let result = SchedulerProtocolCommandResult {
                decision: decision.clone(),
                conflict: outcome.conflict,
                transitions: outcome.outcome.transitions,
                diagnostics: outcome.outcome.diagnostics,
                fact_references: decision_fact_references(
                    &decision,
                    rollout_command_fact_references(command, &outcome.outcome.snapshot.rollout),
                ),
                pre_state_fence: rollout_fence(&rollout),
                post_state_fence: rollout_fence(&outcome.outcome.snapshot.rollout),
            };
            insert_rollout_command_result_tx(
                tx,
                command_kind,
                command_identity,
                &payload_hash,
                &result,
            )?;
            inject_fault(fault, TransitionFaultPoint::BeforeCommit)?;

            Ok(CommandTransactionOutcome::Commit(
                SchedulerRolloutTransitionCommit {
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

fn decision_fact_references(decision: &Decision, references: Vec<String>) -> Vec<String> {
    if *decision == Decision::Rejected {
        Vec::new()
    } else {
        references
    }
}

fn rollout_snapshot(rollout: RolloutState) -> Snapshot {
    Snapshot {
        slot: ActivationSlot::Idle,
        dispatch: AgentDispatchState::Open,
        dispatch_revision: 0,
        focus: None,
        work: BTreeMap::new(),
        waits: BTreeMap::new(),
        activations: BTreeMap::new(),
        activation_authorities: BTreeMap::new(),
        activation_admissions: BTreeMap::new(),
        settlements: BTreeMap::new(),
        missing_settlements: BTreeMap::new(),
        rollout,
        admitted_generations: BTreeSet::new(),
        continuation_admissions: BTreeMap::new(),
    }
}

fn rollout_fence(rollout: &RolloutState) -> serde_json::Value {
    serde_json::json!({
        "config_revision": rollout.config_revision,
        "latest_preflight_revision": rollout.latest_preflight_revision,
        "manifest_revision": rollout.manifest.as_ref().map(|manifest| manifest.revision),
    })
}

fn rollout_command_kind(command: &RolloutCommand) -> &'static str {
    match command {
        RolloutCommand::ConfigureProtocol { .. } => "configure_protocol",
        RolloutCommand::OpenPreflight { .. } => "open_rollout_preflight",
        RolloutCommand::CompletePreflight { .. } => "complete_rollout_preflight",
        RolloutCommand::InstallManifest { .. } => "install_rollout_manifest",
        RolloutCommand::ChangeScenarioAuthority { .. } => "change_scenario_authority",
        RolloutCommand::ReportScenarioHardBlocker { .. } => "report_scenario_hard_blocker",
    }
}

fn canonical_rollout_command_hash(command_kind: &str, command: &RolloutCommand) -> Result<String> {
    let canonical = serde_json::to_vec(&serde_json::json!({
        "schema_version": CANONICAL_COMMAND_SCHEMA_VERSION,
        "command_kind": command_kind,
        "command": command,
    }))?;
    Ok(format!("sha256:{:x}", Sha256::digest(canonical)))
}

fn rollout_command_fact_references(
    command: &RolloutCommand,
    rollout: &RolloutState,
) -> Vec<String> {
    match command {
        RolloutCommand::ConfigureProtocol { .. } => vec!["rollout:config".into()],
        RolloutCommand::OpenPreflight { .. } => rollout
            .preflights
            .get(&rollout.latest_preflight_revision)
            .map(|preflight| format!("rollout:preflight:{}", preflight.revision))
            .into_iter()
            .collect(),
        RolloutCommand::CompletePreflight {
            expected_preflight_revision,
            ..
        } => vec![format!("rollout:preflight:{expected_preflight_revision}")],
        RolloutCommand::InstallManifest { manifest, .. } => {
            vec![format!("rollout:manifest:{}", manifest.revision)]
        }
        RolloutCommand::ChangeScenarioAuthority { scenario_class, .. }
        | RolloutCommand::ReportScenarioHardBlocker { scenario_class, .. } => {
            vec![format!("rollout:scenario:{scenario_class}")]
        }
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
        ProtocolCommand::IssueActivationAuthority(command) => {
            ("issue_activation_authority", command.authority_id.clone())
        }
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
        ProtocolCommand::IssueActivationAuthority(command) => {
            vec![format!("activation_authority:{}", command.authority_id)]
        }
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
    }
}

fn validate_command_agent(agent_id: &str, command: &ProtocolCommand) -> Result<()> {
    let command_agent_id = match command {
        ProtocolCommand::IssueActivationAuthority(command) => Some(&command.activation.agent_id),
        ProtocolCommand::AdmitActivation(command) => Some(&command.activation.agent_id),
        ProtocolCommand::SettleActivation(_)
        | ProtocolCommand::RecordMissingSettlement(_)
        | ProtocolCommand::TriggerWait(_) => None,
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
    for authority in snapshot.activation_authorities.values() {
        if authority.activation.agent_id != agent_id {
            bail!(
                "activation authority {} belongs to another agent",
                authority.authority_id
            );
        }
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

fn stored_rollout_command_result_tx(
    tx: &Transaction<'_>,
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
         FROM scheduler_rollout_command_results
         WHERE command_kind = ?1 AND command_identity = ?2",
        params![command_kind, command_identity],
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
            if decision != enum_token(&result.decision)? {
                bail!("stored scheduler rollout decision column disagrees with outcome");
            }
            let stored_conflict = result
                .conflict
                .as_ref()
                .map(|conflict| {
                    Ok::<_, anyhow::Error>((enum_token(&conflict.kind)?, conflict.code.clone()))
                })
                .transpose()?;
            if stored_conflict != conflict_kind.zip(conflict_code) {
                bail!("stored scheduler rollout conflict columns disagree with outcome");
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

fn insert_rollout_command_result_tx(
    tx: &Transaction<'_>,
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
        "INSERT INTO scheduler_rollout_command_results (
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
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
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

fn enum_from_token<T: DeserializeOwned>(token: &str, field: &str) -> Result<T> {
    serde_json::from_value(serde_json::Value::String(token.to_string()))
        .with_context(|| format!("invalid {field} token {token}"))
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
                status,
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

    for (wait_id, wait) in &snapshot.waits {
        let owner_work_item_id = wait
            .generations
            .get(&wait.current_generation)
            .ok_or_else(|| anyhow!("wait {wait_id} has no current generation"))?
            .owner_work_item_id
            .as_str();
        tx.execute(
            "INSERT INTO scheduler_waits (
               agent_id,
               wait_id,
               owner_work_item_id,
               current_generation,
               payload_json,
               updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(agent_id, wait_id) DO UPDATE SET
               owner_work_item_id = excluded.owner_work_item_id,
               current_generation = excluded.current_generation,
               payload_json = excluded.payload_json,
               updated_at = excluded.updated_at",
            params![
                agent_id,
                wait_id,
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
            tx.execute(
                "INSERT INTO scheduler_wait_generations (
                   agent_id,
                   wait_id,
                   generation,
                   owner_work_item_id,
                   lifecycle_state,
                   trigger_id,
                   trigger_generation,
                   consuming_activation_id,
                   payload_json,
                   created_at,
                   updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9, ?9)
                 ON CONFLICT(agent_id, wait_id, generation) DO UPDATE SET
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
                    &record.owner_work_item_id,
                    enum_token(&record.state)?,
                    trigger_id,
                    trigger_generation,
                    serde_json::to_string(record)?,
                    &now,
                ],
            )?;
        }
    }

    for (authority_id, authority) in &snapshot.activation_authorities {
        let work_item_id = activation_work_item_id(&authority.activation)?;
        tx.execute(
            "INSERT INTO scheduler_activation_authorities (
               agent_id,
               authority_id,
               activation_id,
               work_item_id,
               expected_scheduling_generation,
               expected_dispatch_revision,
               consumed_by_activation_id,
               payload_json,
               created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8)
             ON CONFLICT(agent_id, authority_id) DO UPDATE SET
               activation_id = excluded.activation_id,
               work_item_id = excluded.work_item_id,
               expected_scheduling_generation = excluded.expected_scheduling_generation,
               expected_dispatch_revision = excluded.expected_dispatch_revision,
               consumed_by_activation_id = NULL,
               payload_json = excluded.payload_json",
            params![
                agent_id,
                authority_id,
                &authority.activation.id,
                work_item_id,
                to_i64(
                    authority.expected_scheduling_generation,
                    "authority scheduling generation",
                )?,
                to_i64(
                    authority.expected_dispatch_revision,
                    "authority dispatch revision",
                )?,
                serde_json::to_string(authority)?,
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
        tx.execute(
            "INSERT INTO scheduler_activations (
               agent_id,
               activation_id,
               authority_id,
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
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)
             ON CONFLICT(agent_id, activation_id) DO UPDATE SET
               authority_id = excluded.authority_id,
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
                &activation.work_item_id,
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
    }

    for (authority_id, authority) in &snapshot.activation_authorities {
        tx.execute(
            "UPDATE scheduler_activation_authorities
             SET consumed_by_activation_id = ?3, payload_json = ?4
             WHERE agent_id = ?1 AND authority_id = ?2",
            params![
                agent_id,
                authority_id,
                authority.consumed_by.as_deref(),
                serde_json::to_string(authority)?,
            ],
        )?;
    }
    for (wait_id, wait) in &snapshot.waits {
        for (generation, record) in &wait.generations {
            tx.execute(
                "UPDATE scheduler_wait_generations
                 SET consuming_activation_id = ?4, payload_json = ?5
                 WHERE agent_id = ?1 AND wait_id = ?2 AND generation = ?3",
                params![
                    agent_id,
                    wait_id,
                    to_i64(*generation, "wait generation")?,
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
    Ok(())
}

fn persist_rollout_tx(
    tx: &Transaction<'_>,
    previous: &RolloutState,
    next: &RolloutState,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let changed = tx.execute(
        "UPDATE scheduler_protocol_config
         SET protocol_mode = ?1,
             config_revision = ?2,
             latest_preflight_revision = ?3,
             updated_at = ?4
         WHERE config_id = 1
           AND protocol_mode = ?5
           AND config_revision = ?6
           AND latest_preflight_revision = ?7",
        params![
            enum_token(&next.protocol_mode)?,
            to_i64(next.config_revision, "rollout config revision")?,
            to_i64(
                next.latest_preflight_revision,
                "latest rollout preflight revision",
            )?,
            &now,
            enum_token(&previous.protocol_mode)?,
            to_i64(previous.config_revision, "expected rollout config revision")?,
            to_i64(
                previous.latest_preflight_revision,
                "expected latest rollout preflight revision",
            )?,
        ],
    )?;
    if changed != 1 {
        bail!("scheduler rollout config compare-and-swap failed");
    }

    for (revision, preflight) in &next.preflights {
        let manifest_json = preflight
            .manifest
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        match previous.preflights.get(revision) {
            None => {
                tx.execute(
                    "INSERT INTO scheduler_rollout_preflights (
                       preflight_revision,
                       manifest_revision,
                       state,
                       manifest_json,
                       created_at,
                       updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                    params![
                        to_i64(*revision, "rollout preflight revision")?,
                        to_i64(preflight.manifest_revision, "preflight manifest revision")?,
                        enum_token(&preflight.state)?,
                        manifest_json,
                        &now,
                    ],
                )?;
            }
            Some(old) if old == preflight => {}
            Some(old) => {
                let old_manifest_json = old
                    .manifest
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?;
                let changed = tx.execute(
                    "UPDATE scheduler_rollout_preflights
                     SET manifest_revision = ?1,
                         state = ?2,
                         manifest_json = ?3,
                         updated_at = ?4
                     WHERE preflight_revision = ?5
                       AND manifest_revision = ?6
                       AND state = ?7
                       AND manifest_json IS ?8",
                    params![
                        to_i64(preflight.manifest_revision, "preflight manifest revision")?,
                        enum_token(&preflight.state)?,
                        manifest_json,
                        &now,
                        to_i64(*revision, "rollout preflight revision")?,
                        to_i64(
                            old.manifest_revision,
                            "expected preflight manifest revision"
                        )?,
                        enum_token(&old.state)?,
                        old_manifest_json,
                    ],
                )?;
                if changed != 1 {
                    bail!("scheduler rollout preflight compare-and-swap failed");
                }
            }
        }
    }

    if next.manifest != previous.manifest {
        let manifest = next
            .manifest
            .as_ref()
            .ok_or_else(|| anyhow!("scheduler rollout manifest cannot be removed"))?;
        tx.execute(
            "INSERT INTO scheduler_rollout_manifests (
               manifest_revision, preflight_revision, payload_json, installed_at
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                to_i64(manifest.revision, "rollout manifest revision")?,
                to_i64(
                    manifest.preflight_revision,
                    "rollout manifest preflight revision",
                )?,
                serde_json::to_string(manifest)?,
                &now,
            ],
        )?;
    }

    for (scenario_class, authority) in &next.scenarios {
        let manifest_revision = authority
            .manifest_revision
            .map(|revision| to_i64(revision, "scenario manifest revision"))
            .transpose()?;
        let preflight_revision = authority
            .preflight_revision
            .map(|revision| to_i64(revision, "scenario preflight revision"))
            .transpose()?;
        match previous.scenarios.get(scenario_class) {
            None => {
                tx.execute(
                    "INSERT INTO scheduler_scenario_authorities (
                       scenario_class,
                       mode,
                       rollback_target,
                       manifest_revision,
                       preflight_revision,
                       updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        scenario_class,
                        enum_token(&authority.mode)?,
                        enum_token(&authority.rollback_target)?,
                        manifest_revision,
                        preflight_revision,
                        &now,
                    ],
                )?;
            }
            Some(old) if old == authority => {}
            Some(old) => {
                let old_manifest_revision = old
                    .manifest_revision
                    .map(|revision| to_i64(revision, "expected scenario manifest revision"))
                    .transpose()?;
                let old_preflight_revision = old
                    .preflight_revision
                    .map(|revision| to_i64(revision, "expected scenario preflight revision"))
                    .transpose()?;
                let changed = tx.execute(
                    "UPDATE scheduler_scenario_authorities
                     SET mode = ?1,
                         rollback_target = ?2,
                         manifest_revision = ?3,
                         preflight_revision = ?4,
                         updated_at = ?5
                     WHERE scenario_class = ?6
                       AND mode = ?7
                       AND rollback_target = ?8
                       AND manifest_revision IS ?9
                       AND preflight_revision IS ?10",
                    params![
                        enum_token(&authority.mode)?,
                        enum_token(&authority.rollback_target)?,
                        manifest_revision,
                        preflight_revision,
                        &now,
                        scenario_class,
                        enum_token(&old.mode)?,
                        enum_token(&old.rollback_target)?,
                        old_manifest_revision,
                        old_preflight_revision,
                    ],
                )?;
                if changed != 1 {
                    bail!("scheduler scenario authority compare-and-swap failed");
                }
            }
        }
    }

    for blocker in next.hard_blockers.difference(&previous.hard_blockers) {
        tx.execute(
            "INSERT INTO scheduler_scenario_hard_blockers (
               scenario_class,
               blocker_code,
               config_revision,
               manifest_revision,
               preflight_revision,
               trigger_kind,
               action_json,
               created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                &blocker.scenario_class,
                &blocker.blocker_code,
                to_i64(blocker.config_revision, "hard blocker config revision")?,
                to_i64(blocker.manifest_revision, "hard blocker manifest revision")?,
                to_i64(
                    blocker.preflight_revision,
                    "hard blocker preflight revision",
                )?,
                enum_token(&blocker.trigger)?,
                serde_json::to_string(&blocker.action)?,
                &now,
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
    let activation_authorities = load_payload_map::<ActivationAdmissionAuthority>(
        connection,
        "SELECT authority_id, payload_json FROM scheduler_activation_authorities WHERE agent_id = ?1",
        agent_id,
    )?;
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
    let rollout = load_rollout(connection)?;
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
        activation_authorities,
        activation_admissions,
        settlements,
        missing_settlements,
        rollout,
        admitted_generations,
        continuation_admissions,
    };
    validate_agent_partition(agent_id, &snapshot)?;
    scheduler_protocol::assert_invariants(&snapshot)
        .map_err(|error| anyhow!("invalid persisted scheduler protocol snapshot: {error}"))?;
    Ok(snapshot)
}

fn load_rollout(connection: &Connection) -> Result<RolloutState> {
    let (protocol_mode, config_revision, latest_preflight_revision) = connection
        .query_row(
            "SELECT protocol_mode, config_revision, latest_preflight_revision
             FROM scheduler_protocol_config
             WHERE config_id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("scheduler protocol rollout config is not initialized"))?;

    let mut preflight_statement = connection.prepare(
        "SELECT preflight_revision, manifest_revision, state, manifest_json
         FROM scheduler_rollout_preflights
         ORDER BY preflight_revision",
    )?;
    let preflight_rows = preflight_statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    let mut preflights = BTreeMap::new();
    for row in preflight_rows {
        let (revision, manifest_revision, state, manifest_json) = row?;
        let revision = to_u64(revision, "rollout preflight revision")?;
        let manifest = manifest_json
            .as_deref()
            .map(serde_json::from_str::<RolloutManifest>)
            .transpose()?;
        preflights.insert(
            revision,
            RolloutPreflightRecord {
                revision,
                manifest_revision: to_u64(manifest_revision, "preflight manifest revision")?,
                state: enum_from_token::<RolloutPreflightState>(&state, "rollout preflight state")?,
                manifest,
            },
        );
    }

    let manifest = connection
        .query_row(
            "SELECT manifest_revision, preflight_revision, payload_json
             FROM scheduler_rollout_manifests
             ORDER BY manifest_revision DESC
             LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .map(|(manifest_revision, preflight_revision, payload_json)| {
            let manifest: RolloutManifest = serde_json::from_str(&payload_json)?;
            if manifest.revision != to_u64(manifest_revision, "rollout manifest revision")?
                || manifest.preflight_revision
                    != to_u64(preflight_revision, "rollout manifest preflight revision")?
            {
                bail!("rollout manifest columns disagree with payload");
            }
            Ok(manifest)
        })
        .transpose()?;

    let mut scenario_statement = connection.prepare(
        "SELECT
           scenario_class,
           mode,
           rollback_target,
           manifest_revision,
           preflight_revision
         FROM scheduler_scenario_authorities
         ORDER BY scenario_class",
    )?;
    let scenario_rows = scenario_statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<i64>>(4)?,
        ))
    })?;
    let mut scenarios = BTreeMap::new();
    for row in scenario_rows {
        let (scenario_class, mode, rollback_target, manifest_revision, preflight_revision) = row?;
        scenarios.insert(
            scenario_class,
            ScenarioAuthority {
                mode: enum_from_token::<ScenarioMode>(&mode, "scenario authority mode")?,
                rollback_target: enum_from_token::<ScenarioMode>(
                    &rollback_target,
                    "scenario rollback target",
                )?,
                manifest_revision: manifest_revision
                    .map(|revision| to_u64(revision, "scenario manifest revision"))
                    .transpose()?,
                preflight_revision: preflight_revision
                    .map(|revision| to_u64(revision, "scenario preflight revision"))
                    .transpose()?,
            },
        );
    }

    let mut blocker_statement = connection.prepare(
        "SELECT
           scenario_class,
           blocker_code,
           config_revision,
           manifest_revision,
           preflight_revision,
           trigger_kind,
           action_json
         FROM scheduler_scenario_hard_blockers
         ORDER BY
           scenario_class,
           blocker_code,
           config_revision,
           manifest_revision,
           preflight_revision",
    )?;
    let blocker_rows = blocker_statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;
    let mut hard_blockers = BTreeSet::new();
    for row in blocker_rows {
        let (
            scenario_class,
            blocker_code,
            config_revision,
            manifest_revision,
            preflight_revision,
            trigger_kind,
            action_json,
        ) = row?;
        hard_blockers.insert(ScenarioHardBlockerRecord {
            scenario_class,
            blocker_code,
            config_revision: to_u64(config_revision, "hard blocker config revision")?,
            manifest_revision: to_u64(manifest_revision, "hard blocker manifest revision")?,
            preflight_revision: to_u64(preflight_revision, "hard blocker preflight revision")?,
            trigger: enum_from_token::<RollbackTrigger>(&trigger_kind, "hard blocker trigger")?,
            action: serde_json::from_str::<RollbackAction>(&action_json)?,
        });
    }

    Ok(RolloutState {
        protocol_mode: enum_from_token::<ProtocolMode>(&protocol_mode, "protocol mode")?,
        config_revision: to_u64(config_revision, "rollout config revision")?,
        latest_preflight_revision: to_u64(
            latest_preflight_revision,
            "latest rollout preflight revision",
        )?,
        preflights,
        manifest,
        scenarios,
        hard_blockers,
    })
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
        WorkStatus::NeedsSettlement { activation_id } => ("needs_settlement", Some(activation_id)),
        WorkStatus::Paused { hold_id } => ("paused", Some(hold_id)),
        WorkStatus::Terminal => ("terminal", None),
    }
}

fn activation_work_item_id(activation: &scheduler_protocol::AgentActivation) -> Result<&str> {
    match &activation.binding {
        scheduler_protocol::ActivationBinding::WorkItem { work_item_id } => Ok(work_item_id),
        scheduler_protocol::ActivationBinding::WaitOwner {
            owner_work_item_id, ..
        } => Ok(owner_work_item_id),
        _ => bail!(
            "activation {} has no scheduler WorkItem binding",
            activation.id
        ),
    }
}

fn activation_admission_columns(
    admission: &AdmitActivationCommand,
) -> Result<(&'static str, Option<&str>, Option<&str>, Option<i64>)> {
    match &admission.activation.cause {
        ActivationCause::WorkItemRunnable { .. } => Ok(("scheduling", None, None, None)),
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
            ActivationCause::WaitResume { wait_id, .. },
            scheduler_protocol::ActivationBinding::WaitOwner {
                wait_id: bound_wait_id,
                owner_work_item_id,
            },
        ) if wait_id == bound_wait_id => owner_work_item_id,
        (
            ActivationCause::SettlementRecovery { activation_id },
            scheduler_protocol::ActivationBinding::WorkItem { work_item_id },
        ) => {
            return Ok(format!(
                "{work_item_id}:{}:recovery:{activation_id}",
                admission.expected_scheduling_generation
            ));
        }
        _ => bail!(
            "activation {} has no canonical persisted admission fence",
            activation.id
        ),
    };
    Ok(format!(
        "{work_item_id}:{}",
        admission.expected_scheduling_generation
    ))
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
    let (slot_kind, activation_id, work_item_id, admitted_generation, recovery_for) = match slot {
        ActivationSlot::Idle => ("idle", None, None, None, None),
        ActivationSlot::Running {
            activation_id,
            work_item_id,
            admitted_generation,
            recovery_for,
        } => (
            "running",
            Some(activation_id.as_str()),
            Some(work_item_id.as_str()),
            Some(to_i64(*admitted_generation, "slot admitted generation")?),
            recovery_for.as_deref(),
        ),
    };
    tx.execute(
        "INSERT INTO scheduler_agent_slots (
           agent_id,
           slot_kind,
           activation_id,
           work_item_id,
           admitted_generation,
           recovery_for_activation_id,
           updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(agent_id) DO UPDATE SET
           slot_kind = excluded.slot_kind,
           activation_id = excluded.activation_id,
           work_item_id = excluded.work_item_id,
           admitted_generation = excluded.admitted_generation,
           recovery_for_activation_id = excluded.recovery_for_activation_id,
           updated_at = excluded.updated_at",
        params![
            agent_id,
            slot_kind,
            activation_id,
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
    let (slot_kind, activation_id, work_item_id, admitted_generation, recovery_for) = connection
        .query_row(
            "SELECT
               slot_kind,
               activation_id,
               work_item_id,
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
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("scheduler protocol partition {agent_id} is missing slot row"))?;
    match (
        slot_kind.as_str(),
        activation_id,
        work_item_id,
        admitted_generation,
        recovery_for,
    ) {
        ("idle", None, None, None, None) => Ok(ActivationSlot::Idle),
        (
            "running",
            Some(activation_id),
            Some(work_item_id),
            Some(admitted_generation),
            recovery_for,
        ) => Ok(ActivationSlot::Running {
            activation_id,
            work_item_id,
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
        "SELECT wait_id, generation, payload_json
         FROM scheduler_wait_generations
         WHERE agent_id = ?1
         ORDER BY wait_id, generation",
    )?;
    let rows = statement.query_map([agent_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (wait_id, generation, payload_json) = row?;
        let generation = to_u64(generation, "wait generation")?;
        let record: WaitGenerationRecord = serde_json::from_str(&payload_json)?;
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
           work_item_id,
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
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    let mut activations = BTreeMap::new();
    for row in rows {
        let (activation_id, work_item_id, admitted_generation, lifecycle_state, recovery_for) =
            row?;
        let state = match lifecycle_state.as_str() {
            "admitted" | "running" => ActivationState::Running,
            "settled" | "interrupted" | "cancelled" => ActivationState::Settled,
            "settlement_missing" => ActivationState::SettlementMissing,
            _ => bail!("activation {activation_id} has invalid lifecycle state"),
        };
        activations.insert(
            activation_id,
            ActivationRecord {
                work_item_id,
                admitted_generation: to_u64(admitted_generation, "admitted generation")?,
                state,
                recovery_for,
            },
        );
    }
    Ok(activations)
}
