//! Original automatic Memory decoration shared by ready and sparse Stop actors.
use super::*;
use crate::runtime_db::{
    dispatch_memory::{DispatchMemoryBinding, DispatchMemoryCommandPolicy, DispatchMemoryIntent},
    transitions::PreparedInputTransition,
};

fn attach_dispatch_memory_with_binding(
    db: &crate::runtime_db::RuntimeDb,
    prepared: PreparedInputTransition,
    mut binding: impl FnMut(
        &rusqlite::Transaction<'_>,
        &PreparedInputTransition,
    ) -> Result<Option<DispatchMemoryBinding>>,
    now: impl FnOnce() -> chrono::DateTime<Utc>,
) -> Result<PreparedInputTransition> {
    if !prepared.dispatch_memory_candidate() {
        return Ok(prepared);
    }
    match db.transaction(|tx| {
        crate::runtime_db::dispatch_memory::load_original_auto_memory_tx(tx, prepared.descriptor())
    })? {
        crate::runtime_db::dispatch_memory::OriginalAutoMemory::Restore(original) => {
            return prepared.restore_automatic_dispatch_memory(original)
        }
        crate::runtime_db::dispatch_memory::OriginalAutoMemory::PlainReceipt => {
            return Ok(prepared)
        }
        crate::runtime_db::dispatch_memory::OriginalAutoMemory::Unseen => {}
    }
    let binding = db.transaction(|tx| binding(tx, &prepared))?;
    let Some(binding) = binding else {
        return Ok(prepared);
    };
    let policy = prepared.dispatch_memory_policy(&binding);
    if policy == DispatchMemoryCommandPolicy::None {
        return Ok(prepared);
    }
    let frozen = crate::memory::episode_transition::FrozenEpisodeMemoryValues {
        timestamp: now(),
        initial_builder_id: crate::ids::episode_id(),
        fallback_builder_id: crate::ids::episode_id(),
        next_builder_id: crate::ids::episode_id(),
        audit_event_id: crate::ids::audit_event_id(),
    };
    let memory = match policy {
        DispatchMemoryCommandPolicy::Deferred => DispatchMemoryIntent::Deferred { binding, frozen },
        DispatchMemoryCommandPolicy::Finish => DispatchMemoryIntent::Finish { binding, frozen },
        DispatchMemoryCommandPolicy::None => unreachable!("checked memory policy"),
    };
    prepared.with_automatic_dispatch_memory(memory)
}

pub(crate) fn attach_task_stop_phase_memory_to_original_owner(
    db: &crate::runtime_db::RuntimeDb,
    ledger: &super::super::input_evidence_budget::InputEvidenceBudgetLedger,
    reservation: &MutationReservation,
    prepared: PreparedInputTransition,
    now: chrono::DateTime<Utc>,
) -> Result<PreparedInputTransition> {
    let scope = prepared
        .task_stop_phase_scope()
        .context("Memory metadata adapter requires a true TaskStop phase")?;
    anyhow::ensure!(
        scope.agent_id == reservation.agent_id()
            && scope.identity_incarnation == reservation.identity_incarnation()
            && prepared.descriptor().scopes().len() == 1
            && prepared.actual_memory_terminal_turn_id().is_none(),
        "TaskStop Memory differs from its original reserved scope"
    );
    attach_dispatch_memory_with_binding(
        db,
        prepared,
        |tx, prepared| {
            let root = reservation.capture_ready_snapshot()?;
            let scope = prepared
                .task_stop_phase_scope()
                .context("TaskStop Memory lost its original scope")?;
            if let Some(root) = &root {
                anyhow::ensure!(
                    root.token.identity_incarnation == scope.identity_incarnation
                        && root.token.owner_epoch == reservation.owner_epoch()
                        && root.state.id == scope.agent_id,
                    "TaskStop Memory published State differs from original scope"
                );
            }
            crate::runtime_db::task_stop::load_task_stop_phase_memory_binding_tx(
                tx,
                scope,
                root.as_ref().map(|root| root.state.as_ref()),
                ledger,
            )
        },
        || now,
    )
}

impl RuntimeHandle {
    pub(super) fn attach_dispatch_memory_with_binding(
        &self,
        prepared: PreparedInputTransition,
        binding: impl FnMut(
            &rusqlite::Transaction<'_>,
            &PreparedInputTransition,
        ) -> Result<Option<DispatchMemoryBinding>>,
    ) -> Result<PreparedInputTransition> {
        attach_dispatch_memory_with_binding(&self.inner.runtime_db, prepared, binding, || {
            self.now()
        })
    }
}

/// A genuine original producer facet is required; this is not the Stop-phase facade.
pub(crate) fn attach_original_command_terminal_memory_to_owner(
    db: &crate::runtime_db::RuntimeDb,
    ledger: &super::super::input_evidence_budget::InputEvidenceBudgetLedger,
    reservation: &MutationReservation,
    prepared: PreparedInputTransition,
    now: chrono::DateTime<Utc>,
) -> Result<PreparedInputTransition> {
    let intent = prepared
        .original_command_terminal()
        .context("Command Memory needs original producer facet")?;
    anyhow::ensure!(
        intent.original.scope.agent_id == reservation.agent_id()
            && intent.original.scope.identity_incarnation == reservation.identity_incarnation()
            && prepared.descriptor().scopes().len() == 1,
        "Command Memory original reservation differs"
    );
    if intent.mode
        != crate::runtime_db::command_task_terminal::CommandTaskTerminalInstallMode::Current
    {
        return Ok(prepared);
    }
    let material = std::cell::RefCell::new(Vec::new());
    let prepared = attach_dispatch_memory_with_binding(
        db,
        prepared,
        |tx, prepared| {
            let intent = prepared
                .original_command_terminal()
                .context("Command Memory original source lost")?;
            let limits = ledger.limits();
            let request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
                recovery_at: None,
                query: Default::default(),
                capture: super::super::resident_task::TaskCaptureRequest::current(),
                record_limit: usize::try_from(limits.record_limit)?,
                byte_limit: usize::try_from(
                    limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("Command Memory cap overflow")?,
                )?,
            };
            let owner = crate::runtime_db::command_task_terminal::observe_original_owner_tx(
                tx,
                &intent.original.scope,
                &request,
                ledger,
            )?;
            if owner.mode
                != crate::runtime_db::command_task_terminal::CommandTaskTerminalInstallMode::Current
            {
                return Ok(None);
            }
            let (state, state_guard) =
                crate::runtime_db::command_task_terminal::read_original_state_tx(
                    tx,
                    &intent.original.scope,
                    &request,
                    ledger,
                )?;
            let binding_guard =
                crate::runtime_db::command_task_terminal::memory_binding_material_tx(
                    tx,
                    &intent.original.scope,
                    &state,
                    &request,
                    ledger,
                )?;
            let binding = crate::runtime_db::dispatch_memory::load_dispatch_memory_binding_tx(
                tx,
                &intent.original.scope,
                &state,
            )?;
            material.borrow_mut().extend(owner.into_material());
            material.borrow_mut().push(state_guard);
            material.borrow_mut().extend(binding_guard);
            Ok(binding)
        },
        || now,
    )?;
    prepared.with_original_command_material(material.into_inner())
}

/// The other closed producer borrows the same bounded binding reader.
pub(crate) fn attach_original_child_terminal_memory_to_owner(
    db: &crate::runtime_db::RuntimeDb,
    ledger: &super::super::input_evidence_budget::InputEvidenceBudgetLedger,
    reservation: &MutationReservation,
    prepared: PreparedInputTransition,
    now: chrono::DateTime<Utc>,
) -> Result<PreparedInputTransition> {
    let intent = prepared
        .original_child_terminal()
        .context("child Memory needs original producer facet")?;
    anyhow::ensure!(
        intent.original.scope.agent_id == reservation.agent_id()
            && intent.original.scope.identity_incarnation == reservation.identity_incarnation()
            && prepared
                .descriptor()
                .scopes()
                .contains(&intent.original.scope),
        "child Memory original reservation differs"
    );
    if intent.mode != crate::runtime_db::child_task_terminal::ChildTaskTerminalInstallMode::Current
    {
        return Ok(prepared);
    }
    let material = std::cell::RefCell::new(Vec::new());
    let prepared = attach_dispatch_memory_with_binding(
        db,
        prepared,
        |tx, prepared| {
            let intent = prepared
                .original_child_terminal()
                .context("child Memory original source lost")?;
            let limits = ledger.limits();
            let request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
                recovery_at: None,
                query: Default::default(),
                capture: super::super::resident_task::TaskCaptureRequest::current(),
                record_limit: usize::try_from(limits.record_limit)?,
                byte_limit: usize::try_from(
                    limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("child Memory cap overflow")?,
                )?,
            };
            let owner = crate::runtime_db::command_task_terminal::observe_original_owner_tx(
                tx,
                &intent.original.scope,
                &request,
                ledger,
            )?;
            if owner.mode
                != crate::runtime_db::command_task_terminal::CommandTaskTerminalInstallMode::Current
            {
                return Ok(None);
            }
            let (state, state_guard) =
                crate::runtime_db::command_task_terminal::read_original_state_tx(
                    tx,
                    &intent.original.scope,
                    &request,
                    ledger,
                )?;
            let binding_guard =
                crate::runtime_db::command_task_terminal::memory_binding_material_tx(
                    tx,
                    &intent.original.scope,
                    &state,
                    &request,
                    ledger,
                )?;
            let binding = crate::runtime_db::dispatch_memory::load_dispatch_memory_binding_tx(
                tx,
                &intent.original.scope,
                &state,
            )?;
            material.borrow_mut().extend(owner.into_material());
            material.borrow_mut().push(state_guard);
            material.borrow_mut().extend(binding_guard);
            Ok(binding)
        },
        || now,
    )?;
    prepared.with_original_child_material(material.into_inner())
}
