//! The original registered child monitor joins Task, delegation and audit in one ACK.
use super::*;
use crate::runtime_db::input_mutation_receipts::InputTaskDeletionResult;
use crate::{
    runtime::{
        input_evidence_budget::{
            serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        },
        resident_task::{QualifiedTaskFact, TaskFamily, TaskKey},
        FrozenChildTaskTerminal, OriginalChildTaskProducer,
    },
    runtime_db::{
        child_task_terminal as child, command_task_terminal as owner, task_snapshot as tasks,
    },
};
use std::sync::{Arc, Weak};

#[derive(Debug, serde::Serialize)]
pub(crate) struct OriginalChildTerminalIntent {
    pub(crate) original: child::OriginalChildTaskAddress,
    pub(crate) before: Option<owner::OriginalCommandTaskBefore>,
    pub(crate) mode: child::ChildTaskTerminalInstallMode,
    pub(crate) terminal_digest: String,
    pub(crate) requested: Option<(String, String)>,
    pub(crate) no_op: bool,
    #[serde(skip)]
    material: tasks::TaskMaintenanceMaterial,
}
impl OriginalChildTerminalIntent {
    pub(crate) fn material_estimate(&self) -> tasks::TaskMaintenanceMaterial {
        tasks::TaskMaintenanceMaterial {
            bytes: self.material.bytes,
            records: self.material.records,
        }
    }
}
/// Captured under the scope's held owner. It is only a choice of already
/// prepared heads, never an instruction to prepare an unrelated Task domain.
#[derive(Debug)]
pub(crate) struct OriginalChildStatusSelection {
    scope: InputEvidenceScope,
    revision: u64,
    records: Vec<Arc<QualifiedTaskFact>>,
    _material: Vec<Arc<BudgetViewGuard>>,
    bytes: usize,
    count: usize,
}
impl OriginalChildStatusSelection {
    pub(crate) fn from_captured(
        scope: &InputEvidenceScope,
        revision: u64,
        records: &[&Arc<QualifiedTaskFact>],
        inventory: &child::ChildTaskObserverInventory,
        request: &tasks::TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        anyhow::ensure!(
            revision > 0
                && records.iter().all(|record| record.locator.scope == *scope
                    && inventory
                        .rows()
                        .iter()
                        .any(|row| row.task == record.locator)),
            "child Status selection differs from complete dependency inventory"
        );
        let bytes = records
            .len()
            .checked_mul(std::mem::size_of::<Arc<QualifiedTaskFact>>() + 128)
            .and_then(|n| n.checked_add(scope.agent_id.len() + 256))
            .and_then(|n| n.checked_mul(8))
            .context("child selected Task slots overflow")?;
        let count = records
            .len()
            .checked_add(1)
            .and_then(|n| n.checked_mul(8))
            .context("child selected Task count overflow")?;
        let slots =
            owner::charge_material(&inventory.original().scope, bytes, count, request, ledger)?;
        let body = ledger.register_task_view(
            scope,
            InputMemoryCategory::Candidate,
            records.iter().copied(),
        )?;
        Ok(Self {
            scope: scope.clone(),
            revision,
            records: records.iter().map(|record| (*record).clone()).collect(),
            _material: vec![slots, body],
            bytes,
            count,
        })
    }
}
#[derive(Debug, serde::Serialize)]
pub(super) struct PreparedOriginalChildTerminal {
    pub(super) intent: OriginalChildTerminalIntent,
    #[serde(serialize_with = "serialize_choice")]
    choice: Arc<child::OriginalChildDelegationChoice>,
    #[serde(serialize_with = "serialize_inventory")]
    inventory: Arc<child::ChildTaskObserverInventory>,
    #[serde(skip)]
    authorization: Weak<OriginalChildTaskProducer>,
    #[serde(skip)]
    selected: Vec<OriginalChildStatusSelection>,
    #[serde(skip)]
    guards: Vec<Arc<BudgetViewGuard>>,
}
fn serialize_choice<S: serde::Serializer>(
    value: &Arc<child::OriginalChildDelegationChoice>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    serde::Serialize::serialize(value.as_ref(), s)
}
fn serialize_inventory<S: serde::Serializer>(
    value: &Arc<child::ChildTaskObserverInventory>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    serde::Serialize::serialize(value.as_ref(), s)
}
impl PreparedInputTransition {
    pub(crate) fn original_child_terminal(&self) -> Option<&OriginalChildTerminalIntent> {
        let InputTransitionCommand::Task(task) = self.command.as_ref() else {
            return None;
        };
        task.original_child_terminal
            .as_ref()
            .map(|original| &original.intent)
    }
    pub(crate) fn with_original_child_material(
        mut self,
        guards: Vec<Arc<BudgetViewGuard>>,
    ) -> Result<Self> {
        let InputTransitionCommand::Task(task) = self.command.as_mut() else {
            anyhow::bail!("child material requires original Task facade")
        };
        match (
            &mut task.original_child_terminal,
            &mut task.original_child_running,
        ) {
            (Some(original), None) => original.guards.extend(guards),
            (None, Some(original)) => original._guards.extend(guards),
            _ => anyhow::bail!("child material needs exactly one original producer facet"),
        }
        Ok(self)
    }
    pub(crate) fn prepare_original_child_terminal_tx(
        tx: &Transaction<'_>,
        source: &Arc<OriginalChildTaskProducer>,
        frozen: &FrozenChildTaskTerminal,
        inventory: Arc<child::ChildTaskObserverInventory>,
        selected: Vec<OriginalChildStatusSelection>,
        actor_modes: Option<child::OriginalChildActorOwnerModes>,
        request: &tasks::TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        source.validate_material_ledger(ledger)?;
        let view = source.source_view()?;
        view.validate_frozen(frozen)?;
        let original = source.address();
        let scope = &original.scope;
        anyhow::ensure!(
            view.address() == original,
            "child terminal source address differs"
        );
        anyhow::ensure!(
            inventory.original() == original,
            "child terminal inventory crossed its original lease"
        );
        inventory.validate_complete_tx(tx)?;
        let mut phase = crate::runtime_db::task_status_metadata::PhaseMaterial::new(request);
        let (actor_guards, actor_material) = actor_modes
            .map(child::OriginalChildActorOwnerModes::into_material)
            .unwrap_or_else(|| {
                (
                    Vec::new(),
                    tasks::TaskMaintenanceMaterial {
                        bytes: 0,
                        records: 0,
                    },
                )
            });
        phase.include(actor_material.bytes, actor_material.records)?;
        let frozen_material = frozen.material_estimate();
        phase.include(frozen_material.bytes, frozen_material.records)?;
        let inventory_material = inventory.material_estimate();
        phase.include(inventory_material.bytes, inventory_material.records)?;
        for selection in &selected {
            phase.include(selection.bytes, selection.count)?;
        }
        let observation = phase.owner_tx(tx, scope, ledger)?;
        let owner_mode = observation.mode;
        let mut guards = actor_guards;
        guards.extend(observation.into_material());
        let mode: child::ChildTaskTerminalInstallMode = owner_mode.into();
        let mut before = None;
        let mut no_op = false;
        let mut task_will_change = false;
        let mut message = frozen.message.clone();
        let task = if mode == child::ChildTaskTerminalInstallMode::Retired {
            frozen.task.clone()
        } else {
            let key = TaskKey::new(TaskFamily::Task, &original.admitted.key.id);
            let keys = std::collections::BTreeSet::from([key.clone()]);
            let usage = tasks::task_maintenance_material_tx(tx, scope, &keys)?;
            guards.push(owner::charge_material(
                scope,
                usage.bytes,
                usage.records,
                &phase.remaining()?,
                ledger,
            )?);
            phase.include(usage.bytes, usage.records)?;
            let nodes: usize = tx.query_row(
                "SELECT COUNT(*) FROM tasks t,json_tree(t.payload_json) WHERE t.task_id=?1",
                [&key.id],
                |row| row.get(0),
            )?;
            let nodes_bytes = nodes
                .checked_mul(256 * 8)
                .context("child prepared Task nodes overflow")?;
            let node_count = nodes
                .checked_mul(8)
                .context("child prepared Task node count overflow")?;
            guards.push(owner::charge_material(
                scope,
                nodes_bytes,
                node_count,
                &phase.remaining()?,
                ledger,
            )?);
            phase.include(nodes_bytes, node_count)?;
            let proof = tasks::prepare_existing_task_maintenance_tx(tx, scope, &keys)?;
            let row = proof.qualified_task_before(&key.id)?;
            view.validate_generation(row)?;
            let raw = tasks::checkpoint_material_size_on(tx, &key)?
                .context("child qualified Task disappeared")?;
            let actual = tasks::prepare_task_sql_readback_tx(
                tx,
                scope,
                &key.id,
                &phase.remaining()?,
                ledger,
            )?;
            phase.include(
                raw.checked_add(key.id.len() + 256)
                    .and_then(|n| n.checked_mul(8))
                    .context("child prepared Task readback bytes overflow")?,
                8,
            )?;
            anyhow::ensure!(&actual.task == row, "child qualified Task readback changed");
            before = Some(owner::OriginalCommandTaskBefore {
                content_digest: actual.content_digest.clone(),
                projection_digest: actual.projection_digest.clone(),
            });
            guards.push(actual.budget);
            let mut terminal = frozen.task.clone();
            if let Some(bound) = row
                .detail
                .as_ref()
                .and_then(serde_json::Value::as_object)
                .filter(|detail| detail.contains_key("source_message_id"))
            {
                let target = view
                    .target()
                    .context("child terminal source binding lacks original target")?;
                let target_scope = target
                    .target_scope()
                    .context("child terminal source binding lacks original incarnation")?;
                anyhow::ensure!(
                    row.target().is_some_and(|stored| {
                        stored.kind == crate::types::TaskTargetKind::ChildAgent
                            && stored.agent_id == target.target_id()
                            && stored.agent_id == target_scope.agent_id
                            && stored.incarnation == Some(target_scope.identity_incarnation)
                    }) && bound
                        .get("child_agent_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(target.target_id()),
                    "child terminal source binding crossed original target"
                );
                let detail = terminal
                    .detail
                    .as_mut()
                    .and_then(serde_json::Value::as_object_mut)
                    .context("child terminal Task detail absent")?;
                let result_detail = message
                    .metadata
                    .as_mut()
                    .and_then(|metadata| metadata.get_mut("task_detail"))
                    .and_then(serde_json::Value::as_object_mut)
                    .context("child terminal Result task detail absent")?;
                for projection in [&*detail, &*result_detail] {
                    anyhow::ensure!(
                        projection
                            .get("target_identity_incarnation")
                            .is_none_or(|incarnation| {
                                incarnation.as_u64() == Some(target_scope.identity_incarnation)
                            }),
                        "child terminal source binding crossed frozen incarnation"
                    );
                }
                // Enqueue may ACK this exact binding before reporting a publication
                // error. Retain only its monotone fields, not a newer Task body.
                for key in [
                    "source_message_id",
                    "source_turn_id",
                    "child_agent_id",
                    "target",
                ] {
                    let value = bound
                        .get(key)
                        .context("child terminal acknowledged source binding incomplete")?;
                    for projection in [&mut *detail, &mut *result_detail] {
                        anyhow::ensure!(
                            projection.get(key).is_none_or(|existing| existing == value),
                            "supervised_invocation_source_binding_conflict: {key}"
                        );
                        projection.insert(key.into(), value.clone());
                    }
                }
            }
            no_op = crate::runtime::original_task_update_is_ignored(Some(row), &terminal);
            if no_op || crate::runtime::original_task_status_is_terminal(&row.status) {
                row.clone()
            } else {
                // Running may reduce after the physical result. Freeze this
                // original Task revision once; keep the observed Message clock.
                terminal.updated_at = terminal.updated_at.max(row.updated_at);
                task_will_change = repositories::task_transition(row, &terminal)?
                    == repositories::StateTransitionOutcome::Applied;
                terminal
            }
        };
        // The observed physical result and stable source choice are retained even
        // if a later prepare/receipt attempt fails. Never pick a new latest source.
        drop(view);
        let choice =
            child::choose_original_child_delegation_tx(tx, source, &phase.remaining()?, ledger)?;
        let choice_material = choice.material_estimate();
        phase.include(choice_material.bytes, choice_material.records)?;
        if let Some(delegation) = choice.before() {
            let metadata = message
                .metadata
                .as_mut()
                .and_then(serde_json::Value::as_object_mut)
                .context("child terminal metadata absent")?;
            metadata.insert(
                "delegation_id".into(),
                serde_json::json!(delegation.delegation_id),
            );
            metadata.insert(
                "delegation_parent_work_item_id".into(),
                serde_json::json!(delegation.parent_work_item_id),
            );
            metadata.insert(
                "delegation_child_work_item_id".into(),
                serde_json::json!(delegation.child_work_item_id),
            );
        }
        let (mut command, requested) =
            super::task_terminal_business::project_original_child_business_using_phase_tx(
                tx,
                scope,
                task,
                Some(&message),
                frozen.at,
                frozen.take_fault()?,
                owner_mode,
                no_op,
                task_will_change,
                "task_status_updated",
                request,
                ledger,
                &mut guards,
                &mut phase,
            )?;
        let (expected_child, deletion_id) = if mode == child::ChildTaskTerminalInstallMode::Current
            && !no_op
            && source.original_deletion_requested()
        {
            if let Some(deletion) = frozen.deletion.as_ref() {
                let view = source.source_view()?;
                let target = view
                    .target()
                    .and_then(|target| target.target_scope())
                    .ok_or_else(|| {
                        repositories::TerminalChildDeletionAdmissionRejected::new(
                            &deletion.agent_id,
                            "original child identity is unavailable",
                        )
                    })?;
                let bytes:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)+COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_deletion_jobs WHERE agent_id=?1),0)",[&target.agent_id],|row|row.get(0))?;
                guards.push(owner::charge_material(
                    scope,
                    bytes
                        .checked_add(2048)
                        .and_then(|n| n.checked_mul(8))
                        .context("child deletion identity material overflow")?,
                    64,
                    &phase.remaining()?,
                    ledger,
                )?);
                phase.include(
                    bytes
                        .checked_add(2048)
                        .and_then(|n| n.checked_mul(8))
                        .context("child deletion identity material overflow")?,
                    64,
                )?;
                let mut child_owner = phase.owner_tx(tx, target, ledger)?;
                anyhow::ensure!(
                    child_owner.mode != owner::CommandTaskTerminalInstallMode::Retired,
                    repositories::TerminalChildDeletionAdmissionRejected::new(
                        &deletion.agent_id,
                        "original child incarnation is unavailable"
                    )
                );
                let identity = child_owner
                    .identity
                    .take()
                    .context("current child deletion identity missing")?;
                guards.extend(child_owner.into_material());
                anyhow::ensure!(
                    identity.agent_id == deletion.agent_id
                        && identity.incarnation == target.identity_incarnation,
                    repositories::TerminalChildDeletionAdmissionRejected::new(
                        &deletion.agent_id,
                        "original child incarnation changed"
                    )
                );
                command.agent_deletion = Some(deletion.clone());
                (
                    Some(identity),
                    Some(format!("deletion:child-terminal:{}", original.lease_id)),
                )
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };
        let mut group =
            child::terminal_observer_scopes(source, frozen, &inventory, request, ledger)?;
        // Sparse/Noop terminal disposition never writes the child identity.
        if command.agent_deletion.is_none() {
            group.scopes.retain(|candidate| {
                candidate == scope
                    || inventory
                        .rows()
                        .iter()
                        .any(|row| &row.task.scope == candidate)
            });
        }
        phase.include(group.material.bytes, group.material.records)?;
        guards.push(group.guard);
        guards.extend(group.extra_guards);
        let mut material = frozen.material_estimate();
        let mut include = |usage: tasks::TaskMaintenanceMaterial| -> Result<()> {
            material.bytes = material
                .bytes
                .checked_add(usage.bytes)
                .context("child prepared material overflow")?;
            material.records = material
                .records
                .checked_add(usage.records)
                .context("child prepared count overflow")?;
            Ok(())
        };
        include(inventory.material_estimate())?;
        include(choice.material_estimate())?;
        include(group.material)?;
        for selection in &selected {
            include(tasks::TaskMaintenanceMaterial {
                bytes: selection.bytes,
                records: selection.count,
            })?;
        }
        let bytes = usize::try_from(serialized_bytes(&command)?)?
            .checked_mul(8)
            .context("child prepared command copies overflow")?;
        guards.push(owner::charge_material(
            scope,
            bytes,
            64,
            &phase.remaining()?,
            ledger,
        )?);
        phase.include(bytes, 64)?;
        include(tasks::TaskMaintenanceMaterial { bytes, records: 64 })?;
        let remaining = phase.remaining()?;
        material.bytes = request
            .byte_limit
            .min(64 * 1024 * 1024)
            .checked_sub(remaining.byte_limit)
            .context("child prepared material underflow")?;
        material.records = request
            .record_limit
            .min(65_536)
            .checked_sub(remaining.record_limit)
            .context("child prepared count underflow")?;
        let intent = OriginalChildTerminalIntent {
            original: source.address().clone(),
            before,
            mode,
            terminal_digest: frozen.digest()?,
            requested,
            no_op,
            material,
        };
        let mut task = PreparedTaskTransition::new(
            command,
            Default::default(),
            !no_op,
            expected_child,
            deletion_id,
        )?;
        task.original_child_terminal = Some(PreparedOriginalChildTerminal {
            intent,
            choice,
            inventory,
            authorization: Arc::downgrade(source),
            selected,
            guards,
        });
        Self::task(scope.clone(), frozen.nonce.clone(), frozen.at, task)?
            .with_scope_group(group.scopes)
    }
}
fn original(prepared: &PreparedInputTransition) -> Result<&PreparedOriginalChildTerminal> {
    let InputTransitionCommand::Task(task) = prepared.command.as_ref() else {
        anyhow::bail!("child terminal omitted Task facade")
    };
    task.original_child_terminal
        .as_ref()
        .context("original child terminal facet absent")
}

#[cfg(test)]
pub(super) fn inject_original_terminal_conflict_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
) -> Result<()> {
    let original = original(prepared)?;
    let source = original
        .authorization
        .upgrade()
        .context("original conflict source absent")?;
    if !source.take_original_terminal_conflict() {
        return Ok(());
    }
    let InputTransitionCommand::Task(task) = prepared.command.as_ref() else {
        unreachable!()
    };
    let state = task
        .command
        .agent_state
        .as_ref()
        .context("terminal conflict requires its real State CAS")?;
    // Real OCC validation after business and receipt writes, all of which must
    // disappear in this attempt's enclosing rollback.
    tx.execute(
        "UPDATE agent_states SET payload_json=json_set(payload_json,'$.pending',json_extract(payload_json,'$.pending')+1) WHERE agent_id=?1",
        [&original.intent.original.scope.agent_id],
    )?;
    let changed = AgentStateMutation {
        expected: Some(state.record.clone()),
        record: state.record.clone(),
    };
    validate_agent_state_mutation_tx(tx, Some(&changed))
}

pub(super) fn visit_source_audits(
    prepared: &PreparedInputTransition,
    visit: &mut dyn FnMut(
        crate::runtime_db::current_read_writes::CurrentFamily,
        &str,
    ) -> Result<()>,
) -> Result<()> {
    let command = original(prepared)?;
    let source = command
        .authorization
        .upgrade()
        .context("original child producer unavailable")?;
    source.validate_terminal_digest(&command.intent.terminal_digest)?;
    let frozen = source
        .frozen_terminal()?
        .context("child terminal outcome missing")?;
    for event in command.choice.audit().into_iter().chain(&frozen.audits) {
        visit(
            crate::runtime_db::current_read_writes::CurrentFamily::Event,
            &event.id,
        )?;
    }
    Ok(())
}

pub(super) fn validate_current_task_proof(
    prepared: &PreparedInputTransition,
    proof: &tasks::TaskWriteProof,
) -> Result<()> {
    let command = original(prepared)?;
    let source = command
        .authorization
        .upgrade()
        .context("original child producer unavailable")?;
    source.validate_terminal_digest(&command.intent.terminal_digest)?;
    let view = source.source_view()?;
    anyhow::ensure!(
        view.address() == &command.intent.original
            && command.intent.mode == child::ChildTaskTerminalInstallMode::Current,
        "child current Task authority differs"
    );
    let before = command
        .intent
        .before
        .as_ref()
        .context("current child Task before missing")?;
    proof.validate_original_producer_before(
        &command.intent.original.scope,
        &command.intent.original.admitted.key.id,
        &before.content_digest,
        &before.projection_digest,
    )?;
    view.validate_generation(proof.task_before(&command.intent.original.admitted.key.id)?)?;
    let InputTransitionCommand::Task(task) = prepared.command.as_ref() else {
        unreachable!()
    };
    view.validate_generation(&task.command.task)
}
pub(super) fn apply_source_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    material: &mut tasks::read_preparation::TaskReadMaterial<'_>,
) -> Result<Option<child::ChildDelegationTerminalAck>> {
    let original = original(prepared)?;
    let source = original
        .authorization
        .upgrade()
        .context("original child producer unavailable")?;
    original.inventory.validate_complete_tx(tx)?;
    child::apply_original_child_delegation_tx(
        tx,
        &source,
        &original.choice,
        request,
        ledger,
        material,
    )
}

pub(super) struct ChildSourcePhase {
    delegation: Option<child::ChildDelegationTerminalAck>,
    owners: Vec<(InputEvidenceScope, child::ChildTaskTerminalInstallMode)>,
    audit_inserted: bool,
}
pub(super) fn apply_source_phase_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    material: &mut tasks::read_preparation::TaskReadMaterial<'_>,
) -> Result<ChildSourcePhase> {
    let command = original(prepared)?;
    command.inventory.validate_complete_tx(tx)?;
    material.charge_ids(
        prepared
            .descriptor
            .scopes()
            .iter()
            .try_fold(256usize, |n, s| {
                n.checked_add(s.agent_id.len() + 256)
                    .context("child availability group overflow")
            })?,
        prepared.descriptor.scopes().len(),
    )?;
    let mut owners = Vec::with_capacity(prepared.descriptor.scopes().len());
    for scope in prepared.descriptor.scopes() {
        // A scalar length and actual guard precede identity JSON decode. The
        // observer never reads State before proving this exact incarnation.
        let bytes:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)",[&scope.agent_id],|r|r.get(0))?;
        material.charge_ids(
            bytes
                .checked_add(scope.agent_id.len() + 256)
                .and_then(|n| n.checked_mul(2))
                .context("child original owner material overflow")?,
            2,
        )?;
        let owner = owner::observe_original_owner_tx(tx, scope, request, ledger)?;
        let mode: child::ChildTaskTerminalInstallMode = owner.mode.into();
        if scope == &command.intent.original.scope {
            anyhow::ensure!(
                mode == command.intent.mode,
                "child original owner changed before terminal commit"
            );
        }
        // Its temporary guard overlaps the shared guard while decoding; the
        // latter remains live through all related finalizers and whole receipt.
        owners.push((scope.clone(), mode));
    }
    let delegation = apply_source_tx(tx, prepared, request, ledger, material)?;
    let source = command
        .authorization
        .upgrade()
        .context("original child producer unavailable")?;
    let frozen = source
        .frozen_terminal()?
        .context("child terminal outcome missing")?;
    let mut audit_inserted = false;
    for event in &frozen.audits {
        let bytes:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(data_json AS BLOB))+COALESCE(length(CAST(agent_id AS BLOB)),0) FROM audit_events WHERE audit_event_id=?1),0)+COALESCE((SELECT length(CAST(value AS BLOB)) FROM runtime_metadata WHERE key='event_log_epoch'),0)+256",[&event.id],|r|r.get(0))?;
        material.charge_ids(
            bytes
                .checked_mul(2)
                .context("child worktree audit bytes overflow")?,
            2,
        )?;
        let nodes:usize=tx.query_row("SELECT COALESCE((SELECT COUNT(*) FROM audit_events a,json_tree(a.data_json) WHERE a.audit_event_id=?1),0)",[&event.id],|r|r.get(0))?;
        material.charge_ids(
            nodes
                .checked_mul(256)
                .context("child worktree audit nodes overflow")?,
            nodes
                .checked_mul(2)
                .context("child worktree audit count overflow")?,
        )?;
        let (_, inserted) = crate::runtime_db::evidence::append_audit_event_tx(
            tx,
            Some(&command.intent.original.scope.agent_id),
            event,
        )?;
        audit_inserted |= inserted;
    }
    Ok(ChildSourcePhase {
        delegation,
        owners,
        audit_inserted,
    })
}
pub(super) fn finish_related_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
    phase: ChildSourcePhase,
    value: &mut TransitionCommit,
    deletion: Option<Box<InputTaskDeletionResult>>,
    admitted: tasks::TaskMaintenanceMaterial,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<InputMutationResult> {
    let command = original(prepared)?;
    let mut material = tasks::read_preparation::TaskReadMaterial::new(
        &command.intent.original.scope,
        request,
        Some(ledger),
    )
    .with_memory_budget(memory)
    .with_admitted_material(admitted)?;
    material.charge_ids(
        phase.owners.iter().try_fold(256usize, |n, (s, _)| {
            n.checked_add(s.agent_id.len() + 256)
                .context("child scope ACK overflow")
        })?,
        phase.owners.len(),
    )?;
    let mut scopes = Vec::with_capacity(phase.owners.len());
    for (scope, mode) in phase.owners {
        let mut revision = None;
        if scope == command.intent.original.scope {
            if let Some(delta) = value
                .effects
                .task_delta
                .as_ref()
                .filter(|delta| delta.revision > delta.base_revision)
            {
                revision = Some((delta.base_revision, delta.revision));
            }
        } else if mode == child::ChildTaskTerminalInstallMode::Current {
            if let Some(selected) = command
                .selected
                .iter()
                .find(|selected| selected.scope == scope)
            {
                if let Some(delta) =
                    tasks::status_metadata::refresh_task_status_metadata_using_material_tx(
                        tx,
                        &scope,
                        selected.revision,
                        &selected.records,
                        &command.intent.original.scope,
                        request,
                        ledger,
                        &mut material,
                    )?
                {
                    revision = Some((delta.base_revision, delta.revision));
                    value.effects.related_task_metadata_deltas.push(delta);
                }
            }
        }
        scopes.push(child::ChildTaskStatusScopeAck {
            scope,
            mode,
            base_revision: revision.map(|v| v.0),
            revision: revision.map(|v| v.1),
        });
    }
    let result = match command.intent.mode {
        child::ChildTaskTerminalInstallMode::Current if command.intent.no_op => {
            child::ChildTaskTerminalResult::NoOp
        }
        child::ChildTaskTerminalInstallMode::Current => child::ChildTaskTerminalResult::Delivered {
            message_id: command
                .intent
                .requested
                .as_ref()
                .context("child current terminal delivery absent")?
                .0
                .clone(),
        },
        child::ChildTaskTerminalInstallMode::NoState => {
            child::ChildTaskTerminalResult::Unavailable {
                reason: child::ChildTaskTerminalUnavailable::OwnerNoState,
            }
        }
        child::ChildTaskTerminalInstallMode::Retired => {
            child::ChildTaskTerminalResult::Unavailable {
                reason: child::ChildTaskTerminalUnavailable::ScopeRetired,
            }
        }
    };
    value.applied |= phase.audit_inserted
        || phase
            .delegation
            .as_ref()
            .is_some_and(|ack| ack.source_changed || ack.audit_inserted)
        || !value.effects.related_task_metadata_deltas.is_empty()
        || value
            .effects
            .task_delta
            .as_ref()
            .is_some_and(|delta| delta.revision > delta.base_revision);
    let ack = child::ChildTaskTerminalAck {
        original: command.intent.original.clone(),
        install_mode: command.intent.mode,
        task_changed: value.effects.task_write_keys.contains(&TaskKey::new(
            TaskFamily::Task,
            &command.intent.original.admitted.key.id,
        )),
        result,
        deletion,
        delegation: phase.delegation,
        status_scopes: scopes,
    };
    value
        .effects
        .task_material_budget_views
        .extend(material.into_guards());
    value
        .effects
        .task_material_budget_views
        .extend(command.guards.iter().cloned());
    Ok(InputMutationResult::ChildTaskTerminal { ack: Box::new(ack) })
}
pub(super) fn related_install_keys(
    prepared: &PreparedInputTransition,
    scope: &InputEvidenceScope,
) -> crate::runtime_db::input_recovery_install::InputMutationInstallKeys {
    let command = original(prepared).expect("validated original child terminal");
    let mut keys = crate::runtime_db::input_recovery_install::InputMutationInstallKeys::default();
    if let Some(selected) = command
        .selected
        .iter()
        .find(|selected| &selected.scope == scope)
    {
        keys.tasks.extend(
            selected
                .records
                .iter()
                .map(|record| record.locator.key.id.clone()),
        );
    }
    keys
}

pub(super) fn prepare_input_view_tx<'tx, 'connection, 'command>(
    tx: &'tx Transaction<'connection>,
    prepared: &'command PreparedInputTransition,
    proof: Option<&tasks::TaskWriteProof>,
) -> Result<Option<owner::OriginalCommandTaskInputView<'tx, 'connection, 'command>>> {
    let InputTransitionCommand::Task(task) = prepared.command.as_ref() else {
        return Ok(None);
    };
    let Some(original) = task.original_child_terminal.as_ref() else {
        return Ok(None);
    };
    if task.command.task_result_settlement.is_some() || task.command.message_evidence.is_empty() {
        return Ok(None);
    }
    let [message] = task.command.message_evidence.as_slice() else {
        anyhow::bail!("child producer has multiple original terminal sources")
    };
    let source = original
        .authorization
        .upgrade()
        .context("original child source unavailable")?;
    owner::OriginalCommandTaskInputView::prepare_child_tx(
        tx,
        &source,
        proof.context("child input omitted W1 proof")?,
        &task.command.task,
        message,
        &original.intent.original.scope,
        original
            .intent
            .before
            .as_ref()
            .context("child input before absent")?,
        &original.intent.terminal_digest,
        &original
            .intent
            .requested
            .as_ref()
            .context("child requested Message absent")?
            .1,
    )
    .map(Some)
}

pub(super) fn validate_reserved_scope(
    prepared: &PreparedInputTransition,
    scope: &InputEvidenceScope,
) -> Result<()> {
    let command = original(prepared)?;
    let scopes = prepared.descriptor.scopes();
    let rows = command.inventory.rows();
    let indexed = rows
        .iter()
        .enumerate()
        .filter(|(i, row)| *i == 0 || rows[*i - 1].task.scope != row.task.scope)
        .count();
    let primary_indexed = rows
        .iter()
        .any(|row| row.task.scope == command.intent.original.scope);
    let deletion_extra = prepared.task_deletion_scope().is_some_and(|child| {
        child != &command.intent.original.scope && !rows.iter().any(|row| &row.task.scope == child)
    });
    anyhow::ensure!(
        scopes.len() == indexed + usize::from(!primary_indexed) + usize::from(deletion_extra)
            && rows.iter().all(|row| scopes
                .binary_search_by(
                    |candidate| (&candidate.agent_id, candidate.identity_incarnation).cmp(&(
                        &row.task.scope.agent_id,
                        row.task.scope.identity_incarnation
                    ))
                )
                .is_ok()),
        "child terminal omitted a complete dependent owner"
    );
    anyhow::ensure!(
        scope == &command.intent.original.scope
            || command
                .inventory
                .rows()
                .iter()
                .any(|row| &row.task.scope == scope)
            || prepared.task_deletion_scope() == Some(scope),
        "child terminal group has an unrelated owner"
    );
    Ok(())
}
pub(super) struct ChildCurrentEntryMaterial {
    pub(super) usage: tasks::TaskMaintenanceMaterial,
    pub(super) guards: Vec<Arc<BudgetViewGuard>>,
}
/// Pure lengths precede the generic scope/State validators' first decode.
/// The same estimate is absorbed by W1 before any Task materialization.
pub(super) fn preflight_current_entry_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<ChildCurrentEntryMaterial> {
    let original = original(prepared)?;
    anyhow::ensure!(
        original.intent.mode == child::ChildTaskTerminalInstallMode::Current,
        "child Current entry has no original Current source"
    );
    let mut material = tasks::read_preparation::TaskReadMaterial::new(
        &original.intent.original.scope,
        request,
        Some(ledger),
    )
    .with_admitted_material(original.intent.material_estimate())?;
    for scope in prepared.descriptor.scopes() {
        let bytes:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)",[&scope.agent_id],|row|row.get(0))?;
        material.charge_ids(
            bytes
                .checked_add(scope.agent_id.len() + 256)
                .and_then(|n| n.checked_mul(2))
                .context("child entry identity material overflow")?,
            2,
        )?;
    }
    let bytes:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+COALESCE(length(CAST(current_run_id AS BLOB)),0) FROM agent_states WHERE agent_id=?1),0)",[&original.intent.original.scope.agent_id],|row|row.get(0))?;
    material.charge_ids(
        bytes
            .checked_add(256)
            .and_then(|n| n.checked_mul(2))
            .context("child entry State material overflow")?,
        2,
    )?;
    let state_nodes: usize = tx.query_row(
        "SELECT COUNT(*) FROM agent_states s,json_tree(s.payload_json) WHERE s.agent_id=?1",
        [&original.intent.original.scope.agent_id],
        |row| row.get(0),
    )?;
    material.charge_ids(
        state_nodes
            .checked_mul(2 * 256)
            .context("child entry State nodes overflow")?,
        state_nodes
            .checked_mul(2)
            .context("child entry State count overflow")?,
    )?;
    let key = &original.intent.original.admitted.key;
    let bytes = tasks::checkpoint_material_size_on(tx, key)?.context("child entry Task missing")?;
    material.charge_record(key, bytes)?;
    let nodes: usize = tx.query_row(
        "SELECT COUNT(*) FROM tasks t,json_tree(t.payload_json) WHERE t.task_id=?1",
        [&key.id],
        |row| row.get(0),
    )?;
    material.charge_ids(
        nodes
            .checked_mul(2 * 256)
            .context("child entry Task nodes overflow")?,
        nodes
            .checked_mul(2)
            .context("child entry Task count overflow")?,
    )?;
    let admitted = original.intent.material_estimate();
    let total = material.material_estimate();
    Ok(ChildCurrentEntryMaterial {
        usage: tasks::TaskMaintenanceMaterial {
            bytes: total
                .bytes
                .checked_sub(admitted.bytes)
                .context("child entry bytes underflow")?,
            records: total
                .records
                .checked_sub(admitted.records)
                .context("child entry records underflow")?,
        },
        guards: material.into_guards(),
    })
}

pub(super) fn replay_or_sparse_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Option<InputMutationExecution<TransitionCommit>>> {
    let command = original(prepared)?;
    let mut usage = crate::runtime_db::task_status_metadata::PhaseMaterial::new(request);
    let estimate = command.intent.material_estimate();
    usage.include(estimate.bytes, estimate.records)?;
    let (receipt_guards, bytes, records) = child::child_receipt_material_tx(
        tx,
        prepared.descriptor(),
        &command.intent.original.scope,
        &usage.remaining()?,
        ledger,
    )?;
    usage.include(bytes, records)?;
    if let Some(receipt) = lookup_input_mutation_receipt_tx(tx, prepared.descriptor())? {
        anyhow::ensure!(
            matches!(&receipt.outcome.result,InputMutationResult::ChildTaskTerminal{ack} if ack.original==command.intent.original),
            "child exact replay returned another producer"
        );
        return Ok(Some(InputMutationExecution::Replayed {
            receipt,
            _material_budget_views: receipt_guards,
        }));
    }
    if command.intent.mode == child::ChildTaskTerminalInstallMode::Current {
        return Ok(None);
    }
    let source = command
        .authorization
        .upgrade()
        .context("original child producer unavailable")?;
    source.validate_material_ledger(ledger)?;
    source.validate_terminal_digest(&command.intent.terminal_digest)?;
    let view = source.source_view()?;
    anyhow::ensure!(
        view.address() == &command.intent.original,
        "child sparse source crossed original lease"
    );
    let InputTransitionCommand::Task(task) = prepared.command.as_ref() else {
        unreachable!()
    };
    anyhow::ensure!(
        prepared.memory.is_none()
            && task.stop_intent.is_none()
            && task.original_command_terminal.is_none()
            && task.command.queue_entry.is_none()
            && task.command.message_evidence.is_empty()
            && task.command.task_result_settlement.is_none()
            && task.command.agent_state.is_none()
            && task.command.work_items.is_empty()
            && task.command.wait_conditions.is_empty()
            && task.command.agent_deletion.is_none(),
        "sparse child terminal cannot manufacture ordinary dependencies"
    );
    drop(view);
    let admitted = tasks::TaskMaintenanceMaterial {
        bytes: estimate
            .bytes
            .checked_add(bytes)
            .context("child sparse material overflow")?,
        records: estimate
            .records
            .checked_add(records)
            .context("child sparse count overflow")?,
    };
    let mut material = tasks::read_preparation::TaskReadMaterial::new(
        &command.intent.original.scope,
        request,
        Some(ledger),
    )
    .with_admitted_material(admitted)?;
    let phase = apply_source_phase_tx(tx, prepared, request, ledger, &mut material)?;
    let mut value = TransitionCommit::default();
    if command.intent.mode == child::ChildTaskTerminalInstallMode::NoState {
        let keys = std::collections::BTreeSet::from([TaskKey::new(
            TaskFamily::Task,
            &command.intent.original.admitted.key.id,
        )]);
        let before =
            tasks::task_maintenance_material_tx(tx, &command.intent.original.scope, &keys)?;
        material.charge_ids(
            before
                .bytes
                .checked_div(4)
                .context("child sparse before copies invalid")?,
            before
                .records
                .checked_div(4)
                .unwrap_or(before.records)
                .max(1),
        )?;
        let nodes: usize = tx.query_row(
            "SELECT COUNT(*) FROM tasks t,json_tree(t.payload_json) WHERE t.task_id=?1",
            [&command.intent.original.admitted.key.id],
            |row| row.get(0),
        )?;
        material.charge_ids(
            nodes
                .checked_mul(2 * 256)
                .context("child sparse Task nodes overflow")?,
            nodes
                .checked_mul(2)
                .context("child sparse Task node count overflow")?,
        )?;
        let proof =
            tasks::prepare_existing_task_maintenance_tx(tx, &command.intent.original.scope, &keys)?;
        let actual = proof.qualified_task_before(&command.intent.original.admitted.key.id)?;
        let source_view = source.source_view()?;
        source_view.validate_generation(actual)?;
        source_view.validate_generation(&task.command.task)?;
        let raw_size =
            tasks::checkpoint_material_size_on(tx, &TaskKey::new(TaskFamily::Task, &actual.id))?
                .context("child sparse SQL Task disappeared")?;
        material.charge_record(&TaskKey::new(TaskFamily::Task, &actual.id), raw_size)?;
        let observed = tasks::prepare_task_sql_readback_tx(
            tx,
            &command.intent.original.scope,
            &actual.id,
            request,
            ledger,
        )?;
        anyhow::ensure!(
            command
                .intent
                .before
                .as_ref()
                .is_some_and(|before| before.content_digest == observed.content_digest
                    && before.projection_digest == observed.projection_digest),
            "child sparse original before changed"
        );
        let changed = repositories::upsert_task_tx(tx, &task.command.task)?;
        let actual = if changed {
            keys.clone()
        } else {
            Default::default()
        };
        let ack = tasks::register_existing_task_maintenance_tx(tx, proof, &actual)?;
        value
            .effects
            .task_write_keys
            .extend(ack.qualified_actual_keys);
        value.effects.task_observation_keys.extend(keys);
        value.applied = changed;
        if changed {
            crate::runtime_db::evidence::insert_runtime_index_changes_at_tx(
                tx,
                &task.command.index_changes,
                prepared.descriptor.prepared_at(),
            )?;
        }
        value.effects.notify_memory_index = changed;
        value
            .effects
            .task_material_budget_views
            .push(observed.budget);
        // Original lifecycle audits are part of this same sparse maintenance ACK.
        for event in &task.command.audit_events {
            let size:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(data_json AS BLOB))+COALESCE(length(CAST(agent_id AS BLOB)),0) FROM audit_events WHERE audit_event_id=?1),0)+COALESCE((SELECT length(CAST(value AS BLOB)) FROM runtime_metadata WHERE key='event_log_epoch'),0)+256",[&event.id],|r|r.get(0))?;
            material.charge_ids(
                size.checked_mul(2)
                    .context("child sparse audit bytes overflow")?,
                2,
            )?;
            let nodes:usize=tx.query_row("SELECT COALESCE((SELECT COUNT(*) FROM audit_events a,json_tree(a.data_json) WHERE a.audit_event_id=?1),0)",[&event.id],|r|r.get(0))?;
            material.charge_ids(
                nodes
                    .checked_mul(256)
                    .context("child sparse audit nodes overflow")?,
                nodes
                    .checked_mul(2)
                    .context("child sparse audit count overflow")?,
            )?;
            let (event, inserted) = crate::runtime_db::evidence::append_audit_event_ack_tx(
                tx,
                Some(&command.intent.original.scope.agent_id),
                event,
            )?;
            value.applied |= inserted;
            value.effects.audit_events.push(event);
        }
    }
    let admitted = material.material_estimate();
    value
        .effects
        .task_material_budget_views
        .extend(material.into_guards());
    let result = finish_related_tx(
        tx, prepared, phase, &mut value, None, admitted, request, ledger, None,
    )?;
    value
        .effects
        .task_material_budget_views
        .extend(receipt_guards);
    value.effects.fault = task.command.fault;
    let primary =
        crate::runtime_db::input_recovery_install::InputMutationInstallKeys::from_effects(
            &command.intent.original.scope,
            &value.effects,
        )?;
    let outcome = InputMutationOutcome {
        result,
        applied: value.applied,
        delivery_receipt: None,
        scopes: prepared
            .descriptor
            .scopes()
            .iter()
            .map(|scope| {
                let is_primary = scope == &command.intent.original.scope;
                InputMutationScopeOutcome {
                    scope: scope.clone(),
                    install_keys: if is_primary {
                        primary.clone()
                    } else {
                        related_install_keys(prepared, scope)
                    },
                    audit_events: if is_primary {
                        value.effects.audit_events.clone()
                    } else {
                        Vec::new()
                    },
                    notify_memory_index: is_primary && value.effects.notify_memory_index,
                    notify_scheduler: false,
                }
            })
            .collect(),
    };
    let receipt =
        insert_input_mutation_receipt_tx(tx, prepared.descriptor(), &outcome, Utc::now())?;
    inject_fault(task.command.fault, TransitionFaultPoint::BeforeCommit)?;
    Ok(Some(InputMutationExecution::Committed { value, receipt }))
}
