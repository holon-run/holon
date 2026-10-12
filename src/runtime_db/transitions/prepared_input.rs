//! Original whole-transaction receipts for explicitly frozen transition commands.
//!
//! Prepared commands own their payload exactly once. A replay carries the
//! original outcome only; current installation facts come from a fresh resolver.
use super::*;
use crate::runtime_db::input_mutation_receipts::{
    canonical_command_digest, insert_input_mutation_receipt_tx, lookup_input_mutation_receipt_tx,
    InputMutationFamily, InputMutationOutcome, InputMutationReceipt, InputMutationResult,
    InputMutationScopeOutcome, PreparedInputMutation,
};

/// Every argument of the original queue transaction is frozen before its owner
/// ticket begins. No field is inferred from a callback or the current database.
#[derive(Debug, serde::Serialize)]
pub(crate) struct PreparedQueueTransition {
    pub command: QueueTransitionCommand,
    pub execution_protocol: ExecutionProtocolTransition,
    pub wait_transition: Option<QueueWaitTransition>,
    pub task_expectation: Option<TaskExpectation>,
    pub completion: Option<CompletionTransition>,
    pub wait_registration: Option<WaitTransitionCommand>,
    pub terminal_tool_executions: Vec<ToolExecutionRecord>,
    pub extra_wait_conditions: Vec<WaitConditionRecord>,
    pub delivery: Option<AgentMessageDeliveryRecord>,
    pub timer_wake_claim: Option<TimerWakeClaim>,
    pub task_result_settlement:
        Option<crate::runtime_db::task_result_settlement::TaskResultActivationSettlement>,
}
impl PreparedQueueTransition {
    pub(crate) fn new(command: QueueTransitionCommand) -> Self {
        Self {
            command,
            execution_protocol: Default::default(),
            wait_transition: None,
            task_expectation: None,
            completion: None,
            wait_registration: None,
            terminal_tool_executions: Vec::new(),
            extra_wait_conditions: Vec::new(),
            delivery: None,
            timer_wake_claim: None,
            task_result_settlement: None,
        }
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum InputTransitionCommand {
    Posture {
        command: AgentPostureTransitionCommand,
        started_turn: Option<TurnRecord>,
    },
    Queue(PreparedQueueTransition),
    QueueHeadNoProgress {
        command: QueueHeadNoProgressCommand,
        prepared_at: chrono::DateTime<Utc>,
        audit_event_id: String,
    },
    Terminal(TurnTerminalTransitionCommand),
}
impl InputTransitionCommand {
    fn family(&self) -> InputMutationFamily {
        match self {
            Self::Posture { .. } => InputMutationFamily::AgentPosture,
            Self::Queue(_) => InputMutationFamily::Queue,
            Self::QueueHeadNoProgress { .. } => InputMutationFamily::QueueHeadNoProgress,
            Self::Terminal(_) => InputMutationFamily::Terminal,
        }
    }
    fn agent_id(&self) -> &str {
        match self {
            Self::Posture { command, .. } => &command.agent_id,
            Self::Queue(command) => &command.command.agent_id,
            Self::QueueHeadNoProgress { command, .. } => &command.agent_id,
            Self::Terminal(command) => &command.agent_id,
        }
    }
}

/// Deliberately not Clone: the owned command is not copied into its receipt or
/// witness. The witness retains only descriptor + original-attempt completion.
#[derive(Debug)]
pub(crate) struct PreparedInputTransition {
    descriptor: PreparedInputMutation,
    primary_scope: InputEvidenceScope,
    command: Box<InputTransitionCommand>,
}
impl PreparedInputTransition {
    pub(crate) fn posture(
        scope: InputEvidenceScope,
        command_nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: AgentPostureTransitionCommand,
        started_turn: Option<TurnRecord>,
    ) -> Result<Self> {
        anyhow::ensure!(
            command.agent_id == command.agent_state.record.id,
            "agent posture transition identity mismatch"
        );
        if let Some(turn) = started_turn.as_ref() {
            anyhow::ensure!(
                turn.agent_id == command.agent_id
                    && Some(&turn.turn_id) == command.agent_state.record.current_turn_id.as_ref()
                    && turn.turn_index == command.agent_state.record.turn_index
                    && turn.terminal.is_none(),
                "started Turn and posture differ"
            );
        }
        Self::prepare(
            scope,
            command_nonce,
            prepared_at,
            InputTransitionCommand::Posture {
                command,
                started_turn,
            },
        )
    }
    pub(crate) fn queue(
        scope: InputEvidenceScope,
        command_nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: PreparedQueueTransition,
    ) -> Result<Self> {
        Self::prepare(
            scope,
            command_nonce,
            prepared_at,
            InputTransitionCommand::Queue(command),
        )
    }
    /// One bounded defer/quarantine attempt. Its clock and event identity are
    /// frozen before SQL; replay returns its original optional disposition.
    pub(crate) fn queue_head_no_progress(
        scope: InputEvidenceScope,
        command_nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: QueueHeadNoProgressCommand,
    ) -> Result<Self> {
        let audit_event_id = format!("queue-progress:{command_nonce}");
        Self::prepare(
            scope,
            command_nonce,
            prepared_at,
            InputTransitionCommand::QueueHeadNoProgress {
                command,
                prepared_at,
                audit_event_id,
            },
        )
    }
    pub(crate) fn terminal(
        scope: InputEvidenceScope,
        command_nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: TurnTerminalTransitionCommand,
    ) -> Result<Self> {
        Self::prepare(
            scope,
            command_nonce,
            prepared_at,
            InputTransitionCommand::Terminal(command),
        )
    }
    fn prepare(
        scope: InputEvidenceScope,
        command_nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: InputTransitionCommand,
    ) -> Result<Self> {
        anyhow::ensure!(
            scope.agent_id == command.agent_id(),
            "prepared transition differs from its original scope"
        );
        let descriptor = PreparedInputMutation::new(
            command.family(),
            command_nonce,
            vec![scope.clone()],
            canonical_command_digest(&command)?,
            prepared_at,
        )?;
        Ok(Self {
            descriptor,
            primary_scope: scope,
            command: Box::new(command),
        })
    }
    pub(crate) fn descriptor(&self) -> &PreparedInputMutation {
        &self.descriptor
    }

    pub(crate) fn owned_payload_bytes(&self) -> Result<u64> {
        Ok(crate::runtime::input_evidence_budget::serialized_bytes(
            self.command.as_ref(),
        )?)
    }

    /// Freeze every owner held for this original transaction into its receipt.
    /// Additional owners have no business delta, but share its recovery gate.
    pub(crate) fn with_scope_group(mut self, scopes: Vec<InputEvidenceScope>) -> Result<Self> {
        anyhow::ensure!(
            scopes.contains(&self.primary_scope),
            "prepared transition group omits its original primary scope"
        );
        self.descriptor = PreparedInputMutation::new(
            self.descriptor.family(),
            self.descriptor.command_nonce().to_owned(),
            scopes,
            self.descriptor.command_digest().to_owned(),
            self.descriptor.prepared_at(),
        )?;
        Ok(self)
    }

    // Secondary owners share the original recovery gate, never its write set.
    fn validate_primary_namespace(&self) -> Result<()> {
        let agent = |actual: &str, target: &str| -> Result<()> {
            anyhow::ensure!(
                actual == self.primary_scope.agent_id,
                "prepared input {target} target differs from primary namespace"
            );
            Ok(())
        };
        let state = |mutation: &AgentStateMutation| -> Result<()> {
            agent(&mutation.record.id, "State record")?;
            if let Some(expected) = &mutation.expected {
                agent(&expected.id, "State expected")?;
            }
            Ok(())
        };
        let indexes = |changes: &[RuntimeIndexChange]| -> Result<()> {
            for change in changes {
                agent(&change.agent_id, "index intent")?;
            }
            Ok(())
        };
        let work_items = |mutations: &[WorkItemMutation]| -> Result<()> {
            for mutation in mutations {
                agent(&mutation.record().agent_id, "WorkItem")?;
            }
            Ok(())
        };
        let waits = |records: &[WaitConditionRecord]| -> Result<()> {
            for record in records {
                agent(&record.agent_id, "WaitCondition")?;
            }
            Ok(())
        };
        agent(self.command.agent_id(), "command")?;
        match self.command.as_ref() {
            InputTransitionCommand::Posture {
                command,
                started_turn,
            } => {
                state(&command.agent_state)?;
                if let Some(turn) = started_turn {
                    agent(&turn.agent_id, "started Turn")?;
                }
            }
            InputTransitionCommand::Terminal(command) => {
                state(&command.agent_state)?;
                agent(&command.turn_record.agent_id, "terminal Turn")?;
                for tool in &command.terminal_tool_executions {
                    agent(&tool.agent_id, "terminal Tool")?;
                }
            }
            InputTransitionCommand::QueueHeadNoProgress { command, .. } => {
                agent(&command.expected.agent_id, "Queue expected")?;
                agent(&command.quarantined.agent_id, "Queue record")?;
                state(&command.agent_state)?;
            }
            InputTransitionCommand::Queue(queue) => {
                let command = &queue.command;
                agent(
                    &queue_mutation_record(&command.mutation).agent_id,
                    "Queue record",
                )?;
                if let QueueMutation::CompareAndSet { expected, .. } = &command.mutation {
                    agent(&expected.agent_id, "Queue expected")?;
                }
                if let Some(mutation) = &command.agent_state {
                    state(mutation)?;
                }
                if let Some(expectation) = &command.scheduler_claim_work_item {
                    let record = match expectation {
                        SchedulerClaimWorkItemExpectation::Runnable(record)
                        | SchedulerClaimWorkItemExpectation::BlockedRecheck { record, .. } => {
                            record
                        }
                    };
                    agent(&record.agent_id, "scheduler WorkItem")?;
                }
                for message in &command.message_evidence {
                    agent(&message.agent_id, "Message")?;
                }
                for entry in &command.transcript_entries {
                    agent(&entry.agent_id, "Transcript")?;
                }
                for brief in &command.brief_evidence {
                    agent(&brief.agent_id, "Brief")?;
                }
                if let Some(turn) = &command.turn_record {
                    agent(&turn.agent_id, "Turn")?;
                }
                for tool in &queue.terminal_tool_executions {
                    agent(&tool.agent_id, "terminal Tool")?;
                }
                if let Some(protocol) = &queue.execution_protocol.bootstrap {
                    agent(&protocol.agent_id, "execution bootstrap")?;
                }
                for command in &queue.execution_protocol.commands {
                    if let crate::domain::execution_protocol::ExecutionProtocolCommand::Admit(
                        command,
                    ) = command
                    {
                        agent(&command.attempt.agent_id, "execution Attempt")?;
                    }
                }
                if let Some(expectation) = &queue.task_expectation {
                    agent(&expectation.agent_id, "Task expectation")?;
                }
                if let Some(wait) = &queue.wait_transition {
                    agent(&wait.expected.agent_id, "Wait expected")?;
                    agent(&wait.record.agent_id, "WaitCondition")?;
                    if let Some(work) = &wait.work_item {
                        work_items(std::slice::from_ref(work))?;
                    }
                    indexes(&wait.index_changes)?;
                }
                if let Some(wait) = &queue.wait_registration {
                    agent(&wait.agent_id, "Wait registration")?;
                    work_items(&wait.work_items)?;
                    waits(&wait.wait_conditions)?;
                    for expected in &wait.expected_wait_conditions {
                        agent(&expected.agent_id, "Wait expected")?;
                    }
                    if let Some(mutation) = &wait.agent_state {
                        state(mutation)?;
                    }
                    if let Some(admission) = &wait.task_result_admission {
                        agent(&admission.message.agent_id, "Wait Message")?;
                        agent(&admission.record.agent_id, "Wait Queue record")?;
                        if let Some(expected) = &admission.expected {
                            agent(&expected.agent_id, "Wait Queue expected")?;
                        }
                    }
                    indexes(&wait.index_changes)?;
                }
                waits(&queue.extra_wait_conditions)?;
                if let Some(completion) = &queue.completion {
                    work_items(&completion.work_items)?;
                    waits(&completion.wait_conditions)?;
                    for frame in completion
                        .continuation_expectations
                        .iter()
                        .chain(&completion.continuations)
                    {
                        agent(&frame.agent_id, "continuation")?;
                    }
                    agent(&completion.tool_execution.agent_id, "completion Tool")?;
                    indexes(&completion.index_changes)?;
                }
                if let Some(delivery) = &queue.delivery {
                    agent(&delivery.target_agent_id, "delivery")?;
                }
            }
        }
        Ok(())
    }

    // ID-only mutable targets are checked in the original transaction after
    // receipt replay, so later pruning cannot invalidate an exact original ACK.
    fn validate_primary_targets_tx(&self, tx: &Transaction<'_>) -> Result<()> {
        // These global-ID writers permit owner changes in their original SQL.
        // Check only the finite actual footprint, once per existing target.
        let mut targets = BTreeSet::new();
        match self.command.as_ref() {
            InputTransitionCommand::Queue(queue) => {
                targets.insert((
                    "queue_entries",
                    "message_id",
                    queue_mutation_record(&queue.command.mutation)
                        .message_id
                        .as_str(),
                ));
                if let Some(admission) = queue
                    .wait_registration
                    .as_ref()
                    .and_then(|wait| wait.task_result_admission.as_ref())
                {
                    targets.insert((
                        "queue_entries",
                        "message_id",
                        admission.record.message_id.as_str(),
                    ));
                }
                for work in queue
                    .wait_transition
                    .iter()
                    .filter_map(|wait| wait.work_item.as_ref())
                    .chain(
                        queue
                            .wait_registration
                            .iter()
                            .flat_map(|wait| &wait.work_items),
                    )
                    .chain(
                        queue
                            .completion
                            .iter()
                            .flat_map(|completion| &completion.work_items),
                    )
                {
                    targets.insert(("work_items", "work_item_id", work.record().id.as_str()));
                }
                for wait in queue
                    .wait_transition
                    .iter()
                    .map(|wait| &wait.record)
                    .chain(
                        queue
                            .wait_registration
                            .iter()
                            .flat_map(|wait| &wait.wait_conditions),
                    )
                    .chain(queue.extra_wait_conditions.iter())
                    .chain(
                        queue
                            .completion
                            .iter()
                            .flat_map(|completion| &completion.wait_conditions),
                    )
                {
                    targets.insert(("wait_conditions", "wait_condition_id", wait.id.as_str()));
                }
            }
            InputTransitionCommand::QueueHeadNoProgress { command, .. } => {
                targets.insert((
                    "queue_entries",
                    "message_id",
                    command.quarantined.message_id.as_str(),
                ));
            }
            // Turn, Tool, Continuation and canonical evidence already reject
            // existing foreign owners in their original validators.
            InputTransitionCommand::Posture { .. } | InputTransitionCommand::Terminal(_) => {}
        }
        for (table, column, id) in targets {
            let owner: Option<String> = tx
                .query_row(
                    &format!("SELECT agent_id FROM {table} WHERE {column}=?1"),
                    [id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(owner) = owner {
                anyhow::ensure!(
                    owner == self.primary_scope.agent_id,
                    "prepared input stored {table} target differs from primary namespace"
                );
            }
        }
        let InputTransitionCommand::Queue(queue) = self.command.as_ref() else {
            return Ok(());
        };
        let check = |actual: &str, target: &str| -> Result<()> {
            anyhow::ensure!(
                actual == self.primary_scope.agent_id,
                "prepared input {target} target differs from primary namespace"
            );
            Ok(())
        };
        let record = queue_mutation_record(&queue.command.mutation);
        if let Some(delivery) = delivery_by_message_id_tx(tx, &record.message_id)? {
            check(&delivery.target_agent_id, "stored delivery")?;
        }
        if let Some(candidate) = &queue.delivery {
            if let Some(delivery) =
                crate::runtime_db::agent_message_delivery::delivery_by_idempotency_scope_tx(
                    tx,
                    &candidate.idempotency_scope,
                    &candidate.idempotency_key_digest,
                )?
            {
                check(&delivery.target_agent_id, "stored delivery")?;
            }
        }
        if let Some(claim) = &queue.timer_wake_claim {
            let owner: Option<String> = tx
                .query_row(
                    "SELECT agent_id FROM timers WHERE timer_id=?1",
                    [&claim.timer_id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(owner) = owner {
                check(&owner, "Timer wake")?;
            }
        }
        Ok(())
    }

    /// Potential pending admissions, borrowed from the one frozen command.
    /// Existing queued bodies must be frozen by the caller before preparation;
    /// this method never reads storage or fabricates a SQL-produced body.
    pub(crate) fn pending_messages(&self) -> Result<Vec<&MessageEnvelope>> {
        let InputTransitionCommand::Queue(queue) = self.command.as_ref() else {
            return Ok(Vec::new());
        };
        fn add<'a>(
            primary_scope: &InputEvidenceScope,
            pending: &mut Vec<&'a MessageEnvelope>,
            record: &QueueEntryRecord,
            message: &'a MessageEnvelope,
        ) -> Result<()> {
            anyhow::ensure!(
                record.agent_id == primary_scope.agent_id
                    && message.agent_id == primary_scope.agent_id
                    && record.message_id == message.id,
                "pending admission differs from its original primary scope"
            );
            if let Some(previous) = pending.iter().find(|previous| previous.id == message.id) {
                anyhow::ensure!(
                    *previous == message,
                    "pending admission repeats an ID with a different frozen body"
                );
            } else {
                pending.push(message);
            }
            Ok(())
        }
        let mut pending = Vec::new();
        let record = queue_mutation_record(&queue.command.mutation);
        if matches!(
            record.status,
            QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
        ) {
            let mut matching = queue
                .command
                .message_evidence
                .iter()
                .filter(|message| message.id == record.message_id);
            let message = matching
                .next()
                .context("pending queue mutation requires its frozen message body")?;
            add(&self.primary_scope, &mut pending, record, message)?;
            for duplicate in matching {
                add(&self.primary_scope, &mut pending, record, duplicate)?;
            }
        }
        if let Some(admission) = queue
            .wait_registration
            .as_ref()
            .and_then(|wait| wait.task_result_admission.as_ref())
        {
            anyhow::ensure!(
                queue
                    .wait_registration
                    .as_ref()
                    .is_some_and(|wait| wait.agent_id == self.primary_scope.agent_id),
                "pending wait registration differs from its original primary scope"
            );
            if matches!(
                admission.record.status,
                QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
            ) {
                add(
                    &self.primary_scope,
                    &mut pending,
                    &admission.record,
                    &admission.message,
                )?;
            }
        }
        Ok(pending)
    }
}

/// Business no-op is still Committed: receipt insertion is a real transaction.
/// Replayed never contains an earlier candidate, State, queue, or input delta.
#[derive(Debug)]
pub(crate) enum InputMutationExecution<T> {
    Committed {
        value: T,
        receipt: InputMutationReceipt,
    },
    Replayed {
        receipt: InputMutationReceipt,
    },
}

impl RuntimeTransitionRepository<'_> {
    pub(crate) fn commit_prepared_transition(
        &self,
        prepared: &PreparedInputTransition,
    ) -> Result<InputMutationExecution<TransitionCommit>> {
        let scope = self
            .input_scope
            .as_ref()
            .context("prepared input transition requires an explicit reserved scope")?;
        anyhow::ensure!(
            prepared.primary_scope == *scope
                && prepared.descriptor.scopes().contains(scope)
                && prepared.command.agent_id() == scope.agent_id,
            "prepared input transition scope differs from its repository"
        );
        self.db.transaction(|tx| {
            // Current authorization precedes replay; OCC and canonical aliases
            // follow it. A later current version never invalidates exact replay.
            for reserved_scope in prepared.descriptor.scopes() {
                // An already accepted exact receipt may be replayed during
                // retirement. A missing receipt still needs typed admission.
                if reserved_scope == scope {
                    crate::runtime_db::input_evidence_snapshot::require_original_input_recovery_scope(
                        tx,
                        reserved_scope,
                    )?;
                } else {
                    crate::runtime_db::input_evidence_writes::validate_readonly_input_reservation_tx(
                        tx,
                        reserved_scope,
                    )?;
                }
            }
            prepared.validate_primary_namespace()?;
            if let Some(receipt) = lookup_input_mutation_receipt_tx(tx, &prepared.descriptor)? {
                return Ok(InputMutationExecution::Replayed { receipt });
            }
            prepared.validate_primary_targets_tx(tx)?;
            let facts =
                crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &scope.agent_id)
                    .scope_facts()?;
            let cleanup = if facts.identity.as_ref().is_some_and(|identity| {
                identity.status == crate::types::AgentRegistryStatus::Deleting
            }) {
                Some(prepared.cleanup_proof_tx(tx, scope)?)
            } else {
                None
            };
            let repository = RuntimeTransitionRepository {
                db: self.db,
                input_scope: Some(scope.clone()),
                cleanup_input: cleanup.as_ref(),
            };
            let (value, result) = match prepared.command.as_ref() {
                InputTransitionCommand::Posture {
                    command,
                    started_turn,
                } => (
                    repository.commit_agent_posture_tx(tx, command, started_turn.as_ref())?,
                    InputMutationResult::Transition,
                ),
                InputTransitionCommand::Terminal(command) => (
                    repository.commit_turn_terminal_tx(tx, command)?,
                    InputMutationResult::Transition,
                ),
                InputTransitionCommand::Queue(queue) => (
                    repository.commit_queue_transaction_full_tx(
                        tx,
                        &queue.command,
                        &queue.execution_protocol,
                        queue.wait_transition.as_ref(),
                        queue.task_expectation.as_ref(),
                        queue.completion.as_ref(),
                        queue.wait_registration.as_ref(),
                        &queue.terminal_tool_executions,
                        &queue.extra_wait_conditions,
                        queue.delivery.as_ref(),
                        DeliverySynchronization::Required,
                        queue.timer_wake_claim.as_ref(),
                        queue.task_result_settlement.as_ref(),
                    )?,
                    InputMutationResult::Transition,
                ),
                InputTransitionCommand::QueueHeadNoProgress {
                    command,
                    prepared_at,
                    audit_event_id,
                } => {
                    let original = repository.commit_queue_head_no_progress_tx(
                        tx,
                        command,
                        *prepared_at,
                        audit_event_id,
                    )?;
                    match original {
                        Some(original) => (
                            original.commit,
                            InputMutationResult::QueueHeadNoProgress {
                                outcome: Some(original.outcome),
                            },
                        ),
                        None => (
                            TransitionCommit::default(),
                            InputMutationResult::QueueHeadNoProgress { outcome: None },
                        ),
                    }
                }
            };
            // The original body has already materialized SQL sequences, linked
            // Brief events, persisted index intent, exact count and aliases.
            let install_keys =
                super::super::input_recovery_install::InputMutationInstallKeys::from_effects(
                    scope,
                    &value.effects,
                )?;
            let outcome = InputMutationOutcome {
                result,
                applied: value.applied,
                delivery_receipt: value.delivery_receipt.clone(),
                scopes: prepared
                    .descriptor
                    .scopes()
                    .iter()
                    .map(|reserved_scope| {
                        let primary = reserved_scope == scope;
                        InputMutationScopeOutcome {
                            install_keys: if primary {
                                install_keys.clone()
                            } else {
                                Default::default()
                            },
                            scope: reserved_scope.clone(),
                            audit_events: if primary {
                                value.effects.audit_events.clone()
                            } else {
                                Vec::new()
                            },
                            notify_memory_index: primary && value.effects.notify_memory_index,
                            notify_scheduler: primary && value.effects.notify_scheduler,
                        }
                    })
                    .collect(),
            };
            let receipt =
                insert_input_mutation_receipt_tx(tx, &prepared.descriptor, &outcome, Utc::now())?;
            // A successful no-op still wrote its receipt in this transaction.
            if let InputTransitionCommand::QueueHeadNoProgress { command, .. } =
                prepared.command.as_ref()
            {
                inject_fault(command.fault, TransitionFaultPoint::BeforeCommit)?;
            }
            Ok(InputMutationExecution::Committed { value, receipt })
        })
    }
}

#[cfg(test)]
#[path = "prepared_input_tests.rs"]
mod tests;

impl PreparedInputTransition {
    fn cleanup_proof_tx(
        &self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
    ) -> Result<crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof> {
        use crate::domain::execution_protocol::{
            ExecutionBinding, ExecutionProtocolCommand as Command,
        };
        use crate::runtime::resident_input_evidence::{
            InputEvidenceFamily as Family, InputEvidenceKey as Key,
        };
        let mut roots = BTreeSet::new();
        let mut derived = BTreeSet::new();
        let mut source_ids = BTreeSet::new();
        let mut turn_ids = BTreeSet::new();
        let mut interrupted_return = false;
        let validate_state = |mutation: &AgentStateMutation| -> Result<()> {
            anyhow::ensure!(
                mutation.expected.is_some(),
                "cleanup State requires its original OCC baseline"
            );
            validate_agent_state_mutation_tx(tx, Some(mutation))
        };
        let (transcript, briefs, tools) = match self.command.as_ref() {
            InputTransitionCommand::QueueHeadNoProgress { .. } => {
                bail!("Deleting identity cannot begin a queue-head progress attempt");
            }
            InputTransitionCommand::Posture {
                command,
                started_turn,
            } => {
                anyhow::ensure!(
                    started_turn.is_none(),
                    "Deleting identity cannot start a Turn"
                );
                validate_state(&command.agent_state)?;
                let expected = command
                    .agent_state
                    .expected
                    .as_deref()
                    .context("cleanup State baseline missing")?;
                let next = command.agent_state.record.as_ref();
                anyhow::ensure!(
                    next.turn_index == expected.turn_index
                        && (next.current_run_id.is_none()
                            || next.current_run_id == expected.current_run_id)
                        && (next.current_turn_id.is_none()
                            || next.current_turn_id == expected.current_turn_id)
                        && !matches!(next.status, crate::types::AgentStatus::AwakeRunning)
                        && next.pending <= expected.pending,
                    "Deleting posture cannot create a run, Turn or pending admission"
                );
                return crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof::prepare_tx(tx, scope, roots, derived);
            }
            InputTransitionCommand::Terminal(command) => {
                validate_state(&command.agent_state)?;
                extend_cleanup_turn_tx(
                    tx,
                    scope,
                    &command.turn_record,
                    &mut roots,
                    &mut source_ids,
                    &mut turn_ids,
                )?;
                anyhow::ensure!(
                    !source_ids.is_empty(),
                    "cleanup terminal Turn has no canonical admission source"
                );
                (
                    &[][..],
                    &[][..],
                    command.terminal_tool_executions.as_slice(),
                )
            }
            InputTransitionCommand::Queue(queue) => {
                anyhow::ensure!(
                    matches!(
                        queue.command.operation,
                        QueueOperation::Settle
                            | QueueOperation::Discard
                            | QueueOperation::RepairDrop
                    ),
                    "Deleting identity cannot admit, claim, interject or requeue work"
                );
                anyhow::ensure!(
                    queue.delivery.is_none()
                        && queue.timer_wake_claim.is_none()
                        && queue.command.scheduler_claim_work_item.is_none()
                        && queue
                            .wait_registration
                            .as_ref()
                            .is_none_or(|wait| wait.task_result_admission.is_none()),
                    "cleanup cannot admit a new delivery, claim or task result"
                );
                if let Some(state) = queue.command.agent_state.as_ref() {
                    validate_state(state)?;
                }
                let record = queue_mutation_record(&queue.command.mutation);
                anyhow::ensure!(
                    record.status != QueueEntryStatus::Queued,
                    "cleanup cannot requeue an admission"
                );
                let old: QueueEntryRecord = committed_input_payload_tx(
                    tx,
                    "queue_entries",
                    "message_id",
                    &record.message_id,
                    &scope.agent_id,
                )?;
                anyhow::ensure!(
                    old.agent_id == scope.agent_id,
                    "cleanup queue admission belongs to another agent"
                );
                interrupted_return = record.status == QueueEntryStatus::Interrupted
                    && old.status == QueueEntryStatus::Dequeued;
                source_ids.insert(record.message_id.clone());
                if let Some(turn) = queue.command.turn_record.as_ref() {
                    extend_cleanup_turn_tx(
                        tx,
                        scope,
                        turn,
                        &mut roots,
                        &mut source_ids,
                        &mut turn_ids,
                    )?;
                }
                anyhow::ensure!(
                    queue.command.turn_record.is_none()
                        || source_ids.contains(&record.message_id)
                            && queue.command.turn_record.as_ref().is_some_and(|turn| {
                                turn.input_message_ids.contains(&record.message_id)
                                    || turn.trigger.as_ref().is_some_and(|trigger| {
                                        trigger.message_id.as_deref()
                                            == Some(record.message_id.as_str())
                                    })
                            }),
                    "cleanup terminal Turn differs from its queue admission"
                );
                for message in &queue.command.message_evidence {
                    anyhow::ensure!(
                        message.id == record.message_id && message.agent_id == scope.agent_id,
                        "cleanup cannot append a new Message"
                    );
                }
                for command in &queue.execution_protocol.commands {
                    let attempt_and_outcome = match command {
                        Command::Settle(settle) => {
                            Some((&settle.outcome.attempt_id, &settle.outcome.outcome_id))
                        }
                        Command::Interrupt(interrupt) => {
                            Some((&interrupt.attempt_id, &interrupt.outcome_id))
                        }
                        Command::RegisterWorkItem(_)
                        | Command::Admit(_)
                        | Command::AdvanceWorkItemSourceRevision(_)
                        | Command::RecoverInterruptedTaskResultClaim(_)
                        | Command::RecoverUnadvancedTaskResultClaim(_) => {
                            anyhow::bail!(
                                "cleanup cannot create or recover an execution admission"
                            );
                        }
                        Command::SetWorkItemReadiness(command) => {
                            roots.insert(Key::new(Family::WorkExecution, &command.work_item_id));
                            None
                        }
                        Command::SuspendWorkItemContinuation(command) => {
                            roots.insert(Key::new(Family::WorkExecution, &command.work_item_id));
                            None
                        }
                        Command::ResumeWorkItemContinuation(command) => {
                            roots.insert(Key::new(Family::WorkExecution, &command.work_item_id));
                            None
                        }
                        Command::ReconcileWorkItemContinuationYield(command) => {
                            roots.insert(Key::new(Family::WorkExecution, &command.work_item_id));
                            None
                        }
                        Command::SetWorkItemWaiting(command) => {
                            roots.insert(Key::new(Family::WorkExecution, &command.work_item_id));
                            None
                        }
                        Command::CompleteWorkItem(command) => {
                            roots.insert(Key::new(Family::WorkExecution, &command.work_item_id));
                            None
                        }
                    };
                    if let Some((attempt_id, outcome_id)) = attempt_and_outcome {
                        let attempt: crate::domain::execution_protocol::ExecutionAttempt =
                            committed_input_payload_tx(
                                tx,
                                "execution_protocol_attempts",
                                "attempt_id",
                                attempt_id,
                                &scope.agent_id,
                            )?;
                        let source = attempt.source_message_id.as_deref().or_else(|| {
                            use crate::domain::execution_protocol::ExecutionSourceIdentity as Source;
                            match &attempt.source.identity {
                                Source::QueueMessage { message_id } | Source::InternalFollowup { message_id } => Some(message_id.as_str()),
                                Source::TaskResult { result_message_id, .. } | Source::ChildResult { result_message_id, .. } => Some(result_message_id.as_str()),
                                Source::TriggeredWait { trigger_message_id, .. } => Some(trigger_message_id.as_str()),
                                _ => None,
                            }
                        });
                        anyhow::ensure!(
                            source == Some(record.message_id.as_str()),
                            "cleanup Attempt differs from its original queue admission"
                        );
                        roots.insert(Key::new(Family::Attempt, attempt_id));
                        if let ExecutionBinding::WorkItem { work_item_id } = &attempt.binding {
                            roots.insert(Key::new(Family::WorkExecution, work_item_id));
                        }
                        derived.insert(Key::new(Family::Outcome, outcome_id));
                    }
                }
                if let Some(completion) = queue.completion.as_ref() {
                    anyhow::ensure!(
                        completion
                            .tool_execution
                            .turn_id
                            .as_ref()
                            .is_some_and(|id| turn_ids.contains(id)),
                        "cleanup completion Tool differs from its terminal Turn"
                    );
                    derived.insert(Key::new(
                        Family::ToolExecution,
                        &completion.tool_execution.id,
                    ));
                }
                (
                    queue.command.transcript_entries.as_slice(),
                    queue.command.brief_evidence.as_slice(),
                    queue.terminal_tool_executions.as_slice(),
                )
            }
        };
        let mutation = match self.command.as_ref() {
            InputTransitionCommand::Terminal(command) => Some(&command.agent_state),
            InputTransitionCommand::Queue(queue) => queue.command.agent_state.as_ref(),
            InputTransitionCommand::QueueHeadNoProgress { .. } => {
                anyhow::bail!("new queue progress cannot use an existing admission cleanup proof")
            }
            InputTransitionCommand::Posture { .. } => unreachable!(),
        };
        if let Some(mutation) = mutation {
            let old = mutation
                .expected
                .as_deref()
                .context("cleanup State baseline missing")?;
            let next = mutation.record.as_ref();
            anyhow::ensure!(
                next.turn_index == old.turn_index
                    && (next.current_run_id.is_none() || next.current_run_id == old.current_run_id)
                    && (next.current_turn_id.is_none()
                        || next.current_turn_id == old.current_turn_id
                        || next
                            .current_turn_id
                            .as_ref()
                            .is_some_and(|id| turn_ids.contains(id))),
                "cleanup cannot create a run or a new Turn"
            );
            anyhow::ensure!(
                next.pending <= old.pending.saturating_add(usize::from(interrupted_return)),
                "cleanup cannot add an unrelated pending admission"
            );
        }
        // Root IDs come from the already stored Turn/queue, never a Message's
        // preallocated turn hint or caller-provided arbitrary source refs.
        roots.extend(source_ids.iter().map(|id| Key::new(Family::Message, id)));
        for entry in transcript {
            anyhow::ensure!(
                entry.agent_id == scope.agent_id
                    && entry
                        .related_message_id
                        .as_ref()
                        .is_some_and(|id| source_ids.contains(id)),
                "cleanup Transcript is unrelated to its original admission"
            );
            derived.insert(Key::new(Family::Transcript, &entry.id));
        }
        for brief in briefs {
            anyhow::ensure!(
                brief.agent_id == scope.agent_id
                    && (brief
                        .related_message_id
                        .as_ref()
                        .is_some_and(|id| source_ids.contains(id))
                        || brief
                            .turn_id
                            .as_ref()
                            .is_some_and(|id| turn_ids.contains(id))),
                "cleanup Brief is unrelated to its original admission"
            );
            derived.insert(Key::new(Family::Brief, &brief.id));
        }
        for tool in tools {
            anyhow::ensure!(
                tool.agent_id == scope.agent_id
                    && tool
                        .turn_id
                        .as_ref()
                        .is_some_and(|id| turn_ids.contains(id)),
                "cleanup Tool is unrelated to its original terminal Turn"
            );
            derived.insert(Key::new(Family::ToolExecution, &tool.id));
        }
        let terminal = match self.command.as_ref() {
            InputTransitionCommand::Terminal(command) => Some(&command.turn_record),
            InputTransitionCommand::Queue(queue) => queue.command.turn_record.as_ref(),
            InputTransitionCommand::QueueHeadNoProgress { .. } => {
                anyhow::bail!("new queue progress cannot use an existing admission cleanup proof")
            }
            InputTransitionCommand::Posture { .. } => None,
        };
        if let Some(turn) = terminal {
            for id in &turn.tool_execution_ids {
                let key = Key::new(Family::ToolExecution, id);
                if !derived.contains(&key) {
                    roots.insert(key);
                }
            }
            for id in &turn.produced_brief_ids {
                let key = Key::new(Family::Brief, id);
                if !derived.contains(&key) {
                    roots.insert(key);
                }
            }
        }
        crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof::prepare_tx(
            tx, scope, roots, derived,
        )
    }
}
fn extend_cleanup_turn_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    turn: &TurnRecord,
    roots: &mut BTreeSet<crate::runtime::resident_input_evidence::InputEvidenceKey>,
    source_ids: &mut BTreeSet<String>,
    turn_ids: &mut BTreeSet<String>,
) -> Result<()> {
    use crate::runtime::resident_input_evidence::{
        InputEvidenceFamily as Family, InputEvidenceKey as Key,
    };
    anyhow::ensure!(
        turn.terminal.is_some(),
        "cleanup cannot start or extend an unfinished Turn"
    );
    let old: TurnRecord = committed_input_payload_tx(
        tx,
        "turn_records",
        "turn_id",
        &turn.turn_id,
        &scope.agent_id,
    )?;
    for message_id in &turn.input_message_ids {
        if !old.input_message_ids.contains(message_id) {
            let assigned: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM conversation_input_assignments WHERE agent_id=?1 AND message_id=?2 AND turn_id=?3)",
                params![scope.agent_id, message_id, old.turn_id], |row| row.get(0))?;
            anyhow::ensure!(
                assigned,
                "cleanup cannot introduce a new Turn input assignment"
            );
            source_ids.insert(message_id.clone());
        }
    }
    roots.insert(Key::new(Family::Turn, &old.turn_id));
    turn_ids.insert(old.turn_id.clone());
    source_ids.extend(old.input_message_ids.iter().cloned());
    source_ids.extend(
        old.trigger
            .as_ref()
            .and_then(|trigger| trigger.message_id.clone()),
    );
    let mut statement = tx.prepare("SELECT evidence_id FROM tool_executions WHERE agent_id=?1 AND turn_id=?2 AND json_extract(payload_json,'$.status')='deferred' ORDER BY evidence_id LIMIT 65537")?;
    let ids = statement
        .query_map(params![scope.agent_id, old.turn_id], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    anyhow::ensure!(
        ids.len() <= 65_536,
        "cleanup deferred tools exceed fixed preparation limit"
    );
    roots.extend(
        ids.into_iter()
            .map(|id| Key::new(Family::ToolExecution, id)),
    );
    Ok(())
}
