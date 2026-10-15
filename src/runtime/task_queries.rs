//! Live query selectors belong to their caller, never to durable coverage.
use super::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_input_evidence::InputEvidenceScope,
    resident_task::{TaskFamily, TaskKey, TaskReadError},
};
use anyhow::{Context, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, Weak,
    },
};

/// Preparation roles, independent from active/unsettled mandatory frontiers.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct TaskQuerySelection {
    pub required_keys: BTreeSet<TaskKey>,
    pub output_task_ids: BTreeSet<String>,
    pub reply_message_ids: BTreeSet<String>,
    #[serde(default)]
    pub parent_turn_ids: BTreeSet<String>,
    pub birth_after: Option<u64>,
}

impl TaskQuerySelection {
    pub(crate) fn selector_record_count(&self) -> Result<usize> {
        self.required_keys
            .len()
            .checked_add(self.output_task_ids.len())
            .and_then(|n| n.checked_add(self.reply_message_ids.len()))
            .and_then(|n| n.checked_add(self.parent_turn_ids.len()))
            .and_then(|n| n.checked_add(usize::from(self.birth_after.is_some())))
            .context("Task query selector count overflow")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> InputEvidenceScope {
        InputEvidenceScope {
            agent_id: "query-owner".into(),
            identity_incarnation: 1,
        }
    }
    fn ledger() -> Result<InputEvidenceBudgetLedger> {
        Ok(InputEvidenceBudgetLedger::new(Default::default())?)
    }
    fn output<'a>(id: &'a [&'a str]) -> TaskQuerySpec<'a> {
        TaskQuerySpec {
            output_task_ids: id,
            ..Default::default()
        }
    }

    #[test]
    fn snapshot_keeps_only_exact_live_owner_roles_until_original_use_finishes() -> Result<()> {
        let registry = TaskQueryRegistry::default();
        let ledger = ledger()?;
        let first =
            registry.register(&scope(), 7, output(&["first"]), &ledger, 4096, 1024 * 1024)?;
        let old = registry.register(
            &scope(),
            6,
            output(&["old-epoch"]),
            &ledger,
            4096,
            1024 * 1024,
        )?;
        let foreign = InputEvidenceScope {
            identity_incarnation: 2,
            ..scope()
        };
        let successor = registry.register(
            &foreign,
            7,
            output(&["new-inc"]),
            &ledger,
            4096,
            1024 * 1024,
        )?;
        let frozen = registry.snapshot(&scope(), 7, &ledger, 4096, 1024 * 1024)?;
        assert_eq!(frozen.selection, first.selection().clone());
        assert_eq!(
            frozen.selection.required_keys,
            BTreeSet::from([TaskKey::new(TaskFamily::Task, "first")])
        );
        drop(first);
        assert!(ledger.report()?.owned.metadata_estimate_bytes > 0);
        drop(frozen);
        let next = registry.snapshot(&scope(), 7, &ledger, 4096, 1024 * 1024)?;
        assert_eq!(
            next.selection,
            TaskQuerySelection::default(),
            "durable coverage cannot inherit a dropped query role"
        );
        drop((next, old, successor));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn borrowed_selector_budget_rejects_before_any_lease_or_slot_allocation() -> Result<()> {
        let registry = TaskQueryRegistry::default();
        let ledger = ledger()?;
        let id = "\u{1}".repeat(4096);
        let error = match registry.register(&scope(), 1, output(&[&id]), &ledger, 4096, 4096) {
            Ok(_) => anyhow::bail!("oversize query accepted"),
            Err(error) => error,
        };
        assert!(matches!(
            error.downcast_ref::<TaskReadError>(),
            Some(TaskReadError::ResourceExhausted(_))
        ));
        assert!(registry.0.slots.lock().unwrap().is_empty());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn weak_registry_never_keeps_runtime_or_query_owner_alive() -> Result<()> {
        let registry = TaskQueryRegistry::default();
        let weak = Arc::downgrade(&registry.0);
        let ledger = ledger()?;
        let lease =
            registry.register(&scope(), 1, output(&["held"]), &ledger, 4096, 1024 * 1024)?;
        drop(registry);
        assert!(
            weak.upgrade().is_none(),
            "query must not retain its registry or owner"
        );
        assert_eq!(
            lease.selection().output_task_ids,
            BTreeSet::from(["held".into()])
        );
        drop(lease);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn simultaneous_run_once_sessions_keep_earliest_live_birth_cursor_and_all_roles() -> Result<()>
    {
        let registry = TaskQueryRegistry::default();
        let ledger = ledger()?;
        let required = [TaskKey::new(
            TaskFamily::ResultSettlement,
            "required-result",
        )];
        let first = registry.register(
            &scope(),
            1,
            TaskQuerySpec {
                required_keys: &required,
                output_task_ids: &["output"],
                reply_message_ids: &["reply"],
                parent_turn_ids: &["parent-turn"],
                birth_after: Some(17),
            },
            &ledger,
            4096,
            1024 * 1024,
        )?;
        let second = registry.register(
            &scope(),
            1,
            TaskQuerySpec {
                birth_after: Some(31),
                ..Default::default()
            },
            &ledger,
            4096,
            1024 * 1024,
        )?;
        let both = registry.snapshot(&scope(), 1, &ledger, 4096, 1024 * 1024)?;
        assert_eq!(both.selection.birth_after, Some(17));
        assert_eq!(
            both.selection.reply_message_ids,
            BTreeSet::from(["reply".into()])
        );
        assert!(both.selection.required_keys.contains(&required[0]));
        assert!(both.selection.parent_turn_ids.contains("parent-turn"));
        drop((both, first));
        let remaining = registry.snapshot(&scope(), 1, &ledger, 4096, 1024 * 1024)?;
        assert_eq!(remaining.selection.birth_after, Some(31));
        assert!(remaining.selection.required_keys.is_empty());
        assert!(remaining.selection.parent_turn_ids.is_empty());
        drop((remaining, second));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn contended_last_drop_and_failed_admission_reclaim_dead_slots_before_retry() -> Result<()> {
        let registry = TaskQueryRegistry::default();
        let ledger = ledger()?;
        for use_snapshot in [true, false] {
            let lease = registry.register(
                &scope(),
                1,
                output(&["released"]),
                &ledger,
                4096,
                1024 * 1024,
            )?;
            let slots = registry.0.slots.lock().unwrap();
            drop(lease);
            assert_eq!(slots.len(), 1, "contended last drop leaves a Weak slot");
            assert!(
                ledger.report()?.owned.metadata_estimate_bytes > 0,
                "slot still owns its budget"
            );
            drop(slots);
            let failed = if use_snapshot {
                registry.snapshot(&scope(), 1, &ledger, 1, 1).map(|_| ())
            } else {
                registry
                    .register(&scope(), 1, output(&["reject"]), &ledger, 1, 1)
                    .map(|_| ())
            };
            assert!(failed.is_err());
            assert!(registry.0.slots.lock().unwrap().is_empty());
            assert_eq!(
                ledger.report()?.observed_bytes,
                0,
                "failed admission must not retain dead lease budget"
            );
            let retry =
                registry.register(&scope(), 1, output(&["retry"]), &ledger, 4096, 1024 * 1024)?;
            let read = registry.snapshot(&scope(), 1, &ledger, 4096, 1024 * 1024)?;
            assert_eq!(
                read.selection.output_task_ids,
                BTreeSet::from(["retry".into()])
            );
            drop((read, retry));
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        Ok(())
    }
}

/// Caller-owned IDs are borrowed until all copies and slots are accounted.
#[derive(Default)]
pub(crate) struct TaskQuerySpec<'a> {
    pub required_keys: &'a [TaskKey],
    pub output_task_ids: &'a [&'a str],
    pub reply_message_ids: &'a [&'a str],
    pub parent_turn_ids: &'a [&'a str],
    pub birth_after: Option<u64>,
}

#[derive(Default)]
struct QueryRegistryInner {
    next: AtomicU64,
    slots: Mutex<BTreeMap<u64, QuerySlot>>,
}

struct QuerySlot {
    lease: Weak<QueryLeaseInner>,
    _budget: Arc<BudgetViewGuard>,
}

#[derive(Clone, Default)]
pub(in crate::runtime) struct TaskQueryRegistry(Arc<QueryRegistryInner>);

struct QueryLeaseInner {
    id: u64,
    scope: InputEvidenceScope,
    owner_epoch: u64,
    selection: TaskQuerySelection,
    material: (u64, usize),
    registry: Weak<QueryRegistryInner>,
    _budget: Arc<BudgetViewGuard>,
}
impl Drop for QueryLeaseInner {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            // A snapshot can drop the last upgraded Arc while holding this
            // mutex; it removes dead slots before releasing that same lock.
            if let Ok(mut slots) = registry.slots.try_lock() {
                slots.remove(&self.id);
            };
        }
    }
}

pub(crate) struct TaskQueryLease(Arc<QueryLeaseInner>);
impl TaskQueryLease {
    pub fn selection(&self) -> &TaskQuerySelection {
        &self.0.selection
    }
    pub(crate) fn material_estimate(&self) -> (u64, usize) {
        self.0.material
    }
}

/// Frozen selectors and their original live allocations survive one actor TX.
pub(in crate::runtime) struct TaskQuerySnapshot {
    pub selection: TaskQuerySelection,
    _leases: Vec<Arc<QueryLeaseInner>>,
    _budget: Arc<BudgetViewGuard>,
}

/// Live roles from one actual Runtime instance under its held owner. No root,
/// Runtime, DB or registry backreference escapes with this recovery selection.
pub(crate) struct TaskRecoveryQueries {
    owner_epoch: u64,
    snapshot: TaskQuerySnapshot,
}
impl TaskRecoveryQueries {
    pub(crate) fn selection_for(
        &self,
        scope: &InputEvidenceScope,
        epoch: u64,
    ) -> Option<&TaskQuerySelection> {
        self.selection_for_owner(&scope.agent_id, scope.identity_incarnation, epoch)
    }
    pub(crate) fn selection_for_owner(
        &self,
        agent: &str,
        incarnation: u64,
        epoch: u64,
    ) -> Option<&TaskQuerySelection> {
        (self.owner_epoch == epoch
            && self.snapshot._leases.first().is_some_and(|lease| {
                lease.scope.agent_id == agent && lease.scope.identity_incarnation == incarnation
            }))
        .then_some(&self.snapshot.selection)
    }
}

fn account(
    ledger: &InputEvidenceBudgetLedger,
    scope: &InputEvidenceScope,
    bytes: u64,
    count: usize,
    record_limit: usize,
    byte_limit: usize,
) -> Result<Arc<BudgetViewGuard>> {
    let bytes = bytes
        .checked_mul(8)
        .context("Task query byte copies overflow")?;
    let count = count
        .checked_mul(8)
        .context("Task query record copies overflow")?;
    anyhow::ensure!(
        bytes <= u64::try_from(byte_limit)? && count <= record_limit,
        TaskReadError::ResourceExhausted("live Task query selectors")
    );
    Ok(ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: bytes,
        },
    )?)
}

fn add_id(bytes: &mut u64, count: &mut usize, id: &str) -> Result<()> {
    anyhow::ensure!(!id.is_empty(), "Task query selector ID is empty");
    *bytes = bytes
        .checked_add(serialized_bytes(&id)?)
        .and_then(|n| n.checked_add(256))
        .context("Task query selector bytes overflow")?;
    *count = count
        .checked_add(1)
        .context("Task query selector count overflow")?;
    Ok(())
}

impl TaskQueryRegistry {
    pub(in crate::runtime) fn validate_lease(
        &self,
        lease: &TaskQueryLease,
        scope: &InputEvidenceScope,
        owner_epoch: u64,
    ) -> Result<()> {
        anyhow::ensure!(
            lease.0.scope == *scope
                && lease.0.owner_epoch == owner_epoch
                && Weak::ptr_eq(&lease.0.registry, &Arc::downgrade(&self.0)),
            "Task query lease belongs to another runtime scope or owner"
        );
        Ok(())
    }

    pub fn register(
        &self,
        scope: &InputEvidenceScope,
        owner_epoch: u64,
        spec: TaskQuerySpec<'_>,
        ledger: &InputEvidenceBudgetLedger,
        record_limit: usize,
        byte_limit: usize,
    ) -> Result<TaskQueryLease> {
        anyhow::ensure!(
            scope.identity_incarnation > 0 && owner_epoch > 0,
            "Task query requires an exact published owner"
        );
        let mut bytes = serialized_bytes(scope)?
            .checked_add(1024)
            .context("Task query scope bytes overflow")?;
        let mut count = 1usize;
        add_id(&mut bytes, &mut count, &scope.agent_id)?;
        for key in spec.required_keys {
            add_id(&mut bytes, &mut count, &key.id)?;
        }
        for id in spec.output_task_ids {
            add_id(&mut bytes, &mut count, id)?;
            add_id(&mut bytes, &mut count, id)?; // output role plus its Task root
        }
        for id in spec.reply_message_ids {
            add_id(&mut bytes, &mut count, id)?;
        }
        for id in spec.parent_turn_ids {
            add_id(&mut bytes, &mut count, id)?;
        }
        let mut slots = self
            .0
            .slots
            .lock()
            .map_err(|_| anyhow::anyhow!("Task query registry poisoned"))?;
        slots.retain(|_, value| value.lease.strong_count() > 0);
        let budget = account(ledger, scope, bytes, count, record_limit, byte_limit)?;
        let id = self
            .0
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| anyhow::anyhow!("Task query lease ID exhausted"))?;
        let mut selection = TaskQuerySelection {
            required_keys: spec.required_keys.iter().cloned().collect(),
            output_task_ids: spec
                .output_task_ids
                .iter()
                .map(|id| (*id).to_owned())
                .collect(),
            reply_message_ids: spec
                .reply_message_ids
                .iter()
                .map(|id| (*id).to_owned())
                .collect(),
            parent_turn_ids: spec
                .parent_turn_ids
                .iter()
                .map(|id| (*id).to_owned())
                .collect(),
            birth_after: spec.birth_after,
        };
        selection.required_keys.extend(
            selection
                .output_task_ids
                .iter()
                .map(|id| TaskKey::new(TaskFamily::Task, id)),
        );
        let lease = Arc::new(QueryLeaseInner {
            id,
            scope: scope.clone(),
            owner_epoch,
            selection,
            material: (
                bytes
                    .checked_mul(8)
                    .context("query material bytes overflow")?,
                count
                    .checked_mul(8)
                    .context("query material count overflow")?,
            ),
            registry: Arc::downgrade(&self.0),
            _budget: budget,
        });
        slots.insert(
            id,
            QuerySlot {
                lease: Arc::downgrade(&lease),
                _budget: lease._budget.clone(),
            },
        );
        Ok(TaskQueryLease(lease))
    }

    fn recovery_snapshot(
        &self,
        agent: &str,
        incarnation: u64,
        epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
        record_limit: usize,
        byte_limit: usize,
    ) -> Result<Option<TaskRecoveryQueries>> {
        let first = {
            let mut slots = self
                .0
                .slots
                .lock()
                .map_err(|_| anyhow::anyhow!("Task query registry poisoned"))?;
            slots.retain(|_, value| value.lease.strong_count() > 0);
            let first = slots
                .values()
                .filter_map(|slot| slot.lease.upgrade())
                .find(|lease| {
                    lease.scope.agent_id == agent
                        && lease.scope.identity_incarnation == incarnation
                        && lease.owner_epoch == epoch
                });
            slots.retain(|_, value| value.lease.strong_count() > 0);
            first
        };
        let Some(first) = first else { return Ok(None) };
        let snapshot = self.snapshot(&first.scope, epoch, ledger, record_limit, byte_limit)?;
        Ok(Some(TaskRecoveryQueries {
            owner_epoch: epoch,
            snapshot,
        }))
    }

    pub fn snapshot(
        &self,
        scope: &InputEvidenceScope,
        owner_epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
        record_limit: usize,
        byte_limit: usize,
    ) -> Result<TaskQuerySnapshot> {
        let mut slots = self
            .0
            .slots
            .lock()
            .map_err(|_| anyhow::anyhow!("Task query registry poisoned"))?;
        slots.retain(|_, value| value.lease.strong_count() > 0);
        let mut bytes = serialized_bytes(scope)?
            .checked_add(1024)
            .context("Task query snapshot scope bytes overflow")?;
        let mut count = 1usize;
        // Count borrowed IDs before allocating the merged sets or Arc slots.
        for weak in slots.values() {
            let Some(lease) = weak.lease.upgrade() else {
                continue;
            };
            if lease.scope != *scope || lease.owner_epoch != owner_epoch {
                continue;
            }
            bytes = bytes
                .checked_add(std::mem::size_of::<Arc<QueryLeaseInner>>() as u64)
                .context("Task query snapshot slots overflow")?;
            count = count
                .checked_add(1)
                .context("Task query snapshot count overflow")?;
            for key in &lease.selection.required_keys {
                add_id(&mut bytes, &mut count, &key.id)?;
            }
            for id in &lease.selection.output_task_ids {
                add_id(&mut bytes, &mut count, id)?;
            }
            for id in &lease.selection.reply_message_ids {
                add_id(&mut bytes, &mut count, id)?;
            }
            for id in &lease.selection.parent_turn_ids {
                add_id(&mut bytes, &mut count, id)?;
            }
        }
        // Borrowed upgrades may have held the last Arc while its caller
        // finished. Reclaim those slots before admission and on its error path.
        slots.retain(|_, value| value.lease.strong_count() > 0);
        let budget = match account(ledger, scope, bytes, count, record_limit, byte_limit) {
            Ok(budget) => budget,
            Err(error) => {
                slots.retain(|_, value| value.lease.strong_count() > 0);
                return Err(error);
            }
        };
        let mut selection = TaskQuerySelection::default();
        let mut leases = Vec::new();
        for weak in slots.values() {
            let Some(lease) = weak.lease.upgrade() else {
                continue;
            };
            if lease.scope != *scope || lease.owner_epoch != owner_epoch {
                continue;
            }
            selection
                .required_keys
                .extend(lease.selection.required_keys.iter().cloned());
            selection
                .output_task_ids
                .extend(lease.selection.output_task_ids.iter().cloned());
            selection
                .reply_message_ids
                .extend(lease.selection.reply_message_ids.iter().cloned());
            selection
                .parent_turn_ids
                .extend(lease.selection.parent_turn_ids.iter().cloned());
            if let Some(cursor) = lease.selection.birth_after {
                selection.birth_after =
                    Some(selection.birth_after.map_or(cursor, |old| old.min(cursor)));
            }
            leases.push(lease);
        }
        slots.retain(|_, value| value.lease.strong_count() > 0);
        Ok(TaskQuerySnapshot {
            selection,
            _leases: leases,
            _budget: budget,
        })
    }
}

impl super::RuntimeHandle {
    pub(crate) fn task_recovery_queries(
        &self,
        reservation: &super::mutation_owner::MutationReservation,
        record_limit: usize,
        byte_limit: usize,
    ) -> Result<Option<TaskRecoveryQueries>> {
        reservation.validate_owner(&self.inner.mutation_owner)?;
        self.inner.task_queries.recovery_snapshot(
            reservation.agent_id(),
            reservation.identity_incarnation(),
            reservation.owner_epoch(),
            &self.inner.input_evidence_budget,
            record_limit,
            byte_limit,
        )
    }

    /// Pure registration of an explicit query; acquisition is a separate actor.
    pub(crate) fn register_task_query(&self, spec: TaskQuerySpec<'_>) -> Result<TaskQueryLease> {
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("Task query root is unpublished")?;
        anyhow::ensure!(
            root.state.id == self.inner.mutation_owner.agent_id()
                && root.token.identity_incarnation == self.inner.identity_incarnation,
            "Task query root scope differs"
        );
        let limits = self.inner.input_evidence_budget.limits();
        let byte_limit = limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("Task query request limit overflow")?;
        let scope_bytes = serialized_bytes(&root.state.id)?
            .checked_add(256)
            .and_then(|bytes| bytes.checked_mul(4))
            .context("Task query scope bytes overflow")?;
        anyhow::ensure!(
            scope_bytes <= byte_limit,
            TaskReadError::ResourceExhausted("Task query scope")
        );
        // Account the temporary owned scope before its String clone. The
        // registry's scoped guard then owns the actual retained lease copies.
        let _scope_material = self.inner.input_evidence_budget.register_host_owned_bytes(
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: scope_bytes,
            },
        )?;
        let scope = InputEvidenceScope {
            agent_id: self.inner.mutation_owner.agent_id().to_owned(),
            identity_incarnation: self.inner.identity_incarnation,
        };
        self.inner.task_queries.register(
            &scope,
            root.token.owner_epoch,
            spec,
            &self.inner.input_evidence_budget,
            usize::try_from(limits.record_limit)?,
            usize::try_from(byte_limit)?,
        )
    }
}

/// A RunOnce role is bounded metadata; it retains no Runtime/root/DB.
pub(crate) struct TaskBirthSession {
    cursor: u64,
    lease: TaskQueryLease,
}
impl TaskBirthSession {
    /// Borrow existing guards before a joint caller allocates any selected field.
    pub(in crate::runtime) fn validate_held_material(
        &self,
        runtime: &super::RuntimeHandle,
        root: &Arc<super::mutation_owner::PostureObservation>,
    ) -> Result<()> {
        validate_captured_task_birth_lease(&self.lease.0, self.cursor, runtime, root)
    }
    pub(in crate::runtime) fn cursor(&self) -> u64 {
        self.cursor
    }
    pub(in crate::runtime) fn material_estimate(&self) -> (u64, usize) {
        self.lease.material_estimate()
    }
}
impl std::fmt::Debug for TaskBirthSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskBirthSession")
            .field("cursor", &self.cursor)
            .finish()
    }
}

fn validate_captured_task_birth_lease(
    lease: &Arc<QueryLeaseInner>,
    cursor: u64,
    runtime: &super::RuntimeHandle,
    root: &Arc<super::mutation_owner::PostureObservation>,
) -> Result<()> {
    runtime.inner.mutation_owner.input_queries()?;
    let tasks = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
    let scope = tasks.scope();
    let ledger = &runtime.inner.input_evidence_budget;
    anyhow::ensure!(
        root.state.id == runtime.inner.mutation_owner.agent_id()
            && scope.agent_id == root.state.id
            && scope.identity_incarnation == root.token.identity_incarnation
            && root.token.identity_incarnation == runtime.inner.identity_incarnation
            && root.token.owner_epoch == runtime.inner.mutation_owner.owner_epoch()
            && tasks.shares_budget(ledger),
        "Task birth held scope or ledger differs"
    );
    anyhow::ensure!(
        lease.scope == *scope
            && lease.owner_epoch == root.token.owner_epoch
            && Weak::ptr_eq(
                &lease.registry,
                &Arc::downgrade(&runtime.inner.task_queries.0)
            ),
        "Task query lease belongs to another runtime scope or owner"
    );
    anyhow::ensure!(
        lease.selection.birth_after == Some(cursor),
        TaskReadError::IncompleteQuery
    );
    lease
        ._budget
        .validate_owned_material_binding(scope, ledger, lease.material.0)?;
    // Task's actual slot deliberately shares this same guard. Validate it,
    // without adding the allocation to the prior a second time.
    let slots = runtime
        .inner
        .task_queries
        .0
        .slots
        .lock()
        .map_err(|_| anyhow::anyhow!("Task query registry poisoned"))?;
    let slot = slots
        .get(&lease.id)
        .context("Task birth original slot absent")?;
    anyhow::ensure!(
        Weak::ptr_eq(&slot.lease, &Arc::downgrade(lease))
            && Arc::ptr_eq(&slot._budget, &lease._budget),
        "Task birth original slot differs"
    );
    slot._budget
        .validate_owned_material_binding(scope, ledger, lease.material.0)?;
    Ok(())
}

/// The exact lease and one selected capture phase, never a Root back edge.
pub(crate) struct TaskBirthCaptureMaterial {
    material: (u64, usize),
    _lease: Arc<QueryLeaseInner>,
}

#[cfg(test)]
mod held_material_tests {
    use super::*;
    use crate::runtime::tests::support::LifecycleHarness;

    #[tokio::test]
    async fn captured_task_material_validates_original_slot_and_selected_pins_without_allocation(
    ) -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        runtime
            .quiesce_context_resource_observer_task_for_test()
            .await?;
        let (_, session) = runtime.begin_task_birth_session().await?;
        let at = runtime.now();
        runtime
            .persist_task_transition(
                &crate::types::TaskRecord {
                    id: "actual-captured-task-loan".into(),
                    agent_id: runtime.instance_agent_id().into(),
                    kind: crate::types::TaskKind::CommandTask,
                    status: crate::types::TaskStatus::Queued,
                    created_at: at,
                    updated_at: at,
                    parent_message_id: None,
                    work_item_id: None,
                    summary: None,
                    detail: None,
                    recovery: None,
                },
                "task_queued",
            )
            .await?;
        let root = runtime
            .inner
            .mutation_owner
            .capture()?
            .context("actual Task Root absent")?;
        let ledger = &runtime.inner.input_evidence_budget;
        let before = ledger.report()?.observed_bytes;
        let (selected, material) = runtime.capture_task_birth_session_from_snapshot_with_prior(
            &session,
            &root,
            (64 * 1024 * 1024, 65536),
            (0, 0),
        )?;
        assert!(selected
            .records
            .iter()
            .any(|record| record.locator.key.id == "actual-captured-task-loan"));
        let strong = Arc::strong_count(&root);
        let held = ledger.report()?.observed_bytes;
        runtime.runtime_db().connection()?.execute_batch(
            "ALTER TABLE tasks RENAME TO captured_task_forbidden_tasks;
             ALTER TABLE agent_states RENAME TO captured_task_forbidden_states;",
        )?;
        material.validate_material_binding(runtime, &root, &selected)?;
        assert_eq!(Arc::strong_count(&root), strong);
        assert_eq!(ledger.report()?.observed_bytes, held);
        let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
        let guard = foreign.register_owned_bytes(
            &selected.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: session.material_estimate().0,
                metadata_estimate_bytes: 0,
            },
        )?;
        let original = {
            let mut slots = runtime.inner.task_queries.0.slots.lock().unwrap();
            std::mem::replace(
                &mut slots.get_mut(&session.lease.0.id).unwrap()._budget,
                guard,
            )
        };
        assert!(material
            .validate_material_binding(runtime, &root, &selected)
            .is_err());
        assert_eq!(ledger.report()?.observed_bytes, held);
        let foreign_guard = {
            let mut slots = runtime.inner.task_queries.0.slots.lock().unwrap();
            std::mem::replace(
                &mut slots.get_mut(&session.lease.0.id).unwrap()._budget,
                original,
            )
        };
        drop(foreign_guard);
        assert_eq!(foreign.report()?.observed_bytes, 0);
        material.validate_material_binding(runtime, &root, &selected)?;
        drop((selected, material));
        assert_eq!(ledger.report()?.observed_bytes, before);
        Ok(())
    }

    #[tokio::test]
    async fn task_held_validation_is_pure_and_rejects_a_foreign_actual_slot() -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let _pause = runtime.pause_resource_observer_for_test().await;
        let (_, session) = runtime.begin_task_birth_session().await?;
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        let ledger = &runtime.inner.input_evidence_budget;
        let before = ledger.report()?.observed_bytes;
        let strong = Arc::strong_count(&root);
        runtime.runtime_db().connection()?.execute_batch(
            "ALTER TABLE tasks RENAME TO held_task_forbidden_tasks;
             ALTER TABLE agent_states RENAME TO held_task_forbidden_states;",
        )?;
        session.validate_held_material(runtime, &root)?;
        assert_eq!(Arc::strong_count(&root), strong);
        assert_eq!(ledger.report()?.observed_bytes, before);
        let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
        let actual = foreign.register_owned_bytes(
            root.tasks.as_ref().unwrap().scope(),
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: session.material_estimate().0,
                metadata_estimate_bytes: 0,
            },
        )?;
        // A private adversarial slot, restored before dropping the real lease.
        // It never changes a publication or grants another Task body.
        let original = {
            let mut slots = runtime.inner.task_queries.0.slots.lock().unwrap();
            std::mem::replace(
                &mut slots.get_mut(&session.lease.0.id).unwrap()._budget,
                actual,
            )
        };
        assert!(session.validate_held_material(runtime, &root).is_err());
        assert_eq!(ledger.report()?.observed_bytes, before);
        let actual = {
            let mut slots = runtime.inner.task_queries.0.slots.lock().unwrap();
            std::mem::replace(
                &mut slots.get_mut(&session.lease.0.id).unwrap()._budget,
                original,
            )
        };
        drop(actual);
        assert_eq!(foreign.report()?.observed_bytes, 0);
        session.validate_held_material(runtime, &root)?;
        Ok(())
    }
}
impl TaskBirthCaptureMaterial {
    /// Borrow original lease/slot and selected body guards before using its cost.
    pub(in crate::runtime) fn validate_material_binding(
        &self,
        runtime: &super::RuntimeHandle,
        root: &Arc<super::mutation_owner::PostureObservation>,
        selected: &super::resident_task::CapturedTasks,
    ) -> Result<()> {
        let cursor = self
            ._lease
            .selection
            .birth_after
            .ok_or(TaskReadError::IncompleteQuery)?;
        validate_captured_task_birth_lease(&self._lease, cursor, runtime, root)?;
        let domain = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
        anyhow::ensure!(
            selected.revision == domain.coverage().revision
                && self.material.0 >= self._lease.material.0
                && self.material.1 >= self._lease.material.1,
            "captured Task material or publication differs"
        );
        domain.validate_scheduler_body_loan(selected, &runtime.inner.input_evidence_budget)
    }
    pub(crate) fn material_estimate(&self) -> Result<(u64, usize)> {
        Ok(self.material)
    }
}

pub(crate) struct TaskQueryOwnedRecords {
    _capture: super::resident_task::CapturedTasks,
    _owned: Arc<BudgetViewGuard>,
}
impl super::RuntimeHandle {
    /// Cursor and lease registration share one held published owner, so a
    /// native ACK cannot fall between baseline selection and role admission.
    pub(crate) async fn begin_task_birth_session(
        &self,
    ) -> Result<(Arc<crate::types::AgentState>, TaskBirthSession)> {
        let reservation = self.inner.mutation_owner.reserve().await?;
        let root = reservation
            .capture_ready_snapshot()?
            .context("Task birth session root is unpublished")?;
        let session = self.register_task_birth_session_from_snapshot(&reservation, &root)?;
        Ok((root.state.clone(), session))
    }
    /// The unified report actor registers all domains under this same owner and
    /// exact publication. A stale caller capture cannot move its baseline back.
    pub(in crate::runtime) fn register_task_birth_session_from_snapshot(
        &self,
        reservation: &super::mutation_owner::MutationReservation,
        root: &Arc<super::mutation_owner::PostureObservation>,
    ) -> Result<TaskBirthSession> {
        self.register_task_birth_session_from_snapshot_with_prior(
            reservation,
            root,
            (usize::MAX, usize::MAX),
            (0, 0),
        )
    }
    pub(in crate::runtime) fn register_task_birth_session_from_snapshot_with_prior(
        &self,
        reservation: &super::mutation_owner::MutationReservation,
        root: &Arc<super::mutation_owner::PostureObservation>,
        limits: (usize, usize),
        prior: (u64, usize),
    ) -> Result<TaskBirthSession> {
        reservation.validate_owner(&self.inner.mutation_owner)?;
        let current = reservation
            .capture_ready_snapshot()?
            .context("Task birth session root is unpublished")?;
        anyhow::ensure!(
            Arc::ptr_eq(&current, root)
                && reservation.agent_id() == self.inner.mutation_owner.agent_id()
                && reservation.identity_incarnation() == self.inner.identity_incarnation,
            "Task birth session captured publication differs"
        );
        let tasks = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
        anyhow::ensure!(
            root.state.id == self.inner.mutation_owner.agent_id()
                && tasks.scope().agent_id == root.state.id
                && tasks.scope().identity_incarnation == root.token.identity_incarnation
                && root.token.owner_epoch == reservation.owner_epoch()
                && root.token.identity_incarnation == self.inner.identity_incarnation,
            "Task birth session publication scope differs"
        );
        let cursor = tasks
            .coverage()
            .query
            .birth_cursor
            .ok_or(TaskReadError::IncompleteQuery)?;
        let remaining = super::run_birth_sessions::remaining_material(
            &self.inner.input_evidence_budget,
            limits,
            prior,
        )?;
        let lease = self.inner.task_queries.register(
            tasks.scope(),
            root.token.owner_epoch,
            TaskQuerySpec {
                birth_after: Some(cursor),
                ..Default::default()
            },
            &self.inner.input_evidence_budget,
            remaining.1,
            remaining.0,
        )?;
        Ok(TaskBirthSession { cursor, lease })
    }
    pub(crate) fn capture_task_birth_session(
        &self,
        session: &TaskBirthSession,
    ) -> Result<super::resident_task::CapturedTasks> {
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("Task birth session root is unpublished")?;
        self.capture_task_birth_session_from_snapshot(session, &root)
    }
    /// A coherent report read captures its owner once, then selects every domain
    /// from that immutable publication. Returned material retains no root.
    pub(in crate::runtime) fn capture_task_birth_session_from_snapshot(
        &self,
        session: &TaskBirthSession,
        root: &Arc<super::mutation_owner::PostureObservation>,
    ) -> Result<super::resident_task::CapturedTasks> {
        let limits = self.inner.input_evidence_budget.limits();
        let bytes = usize::try_from(
            limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("Task birth capture limits overflow")?,
        )?;
        self.capture_task_birth_session_from_snapshot_with_prior(
            session,
            root,
            (bytes, usize::try_from(limits.record_limit)?),
            (0, 0),
        )
        .map(|(selected, _)| selected)
    }
    pub(in crate::runtime) fn capture_task_birth_session_from_snapshot_with_prior(
        &self,
        session: &TaskBirthSession,
        root: &Arc<super::mutation_owner::PostureObservation>,
        limits: (usize, usize),
        prior: (u64, usize),
    ) -> Result<(
        super::resident_task::CapturedTasks,
        TaskBirthCaptureMaterial,
    )> {
        session.validate_held_material(self, root)?;
        let tasks = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
        let lease = session.lease.material_estimate();
        let actual_limits = self.inner.input_evidence_budget.limits();
        let actual_bytes = usize::try_from(
            actual_limits
                .history_per_agent_bytes
                .checked_add(actual_limits.pending_per_agent_bytes)
                .context("Task birth actual limits overflow")?,
        )?;
        let limits = (
            limits.0.min(actual_bytes).min(64 * 1024 * 1024),
            limits
                .1
                .min(usize::try_from(actual_limits.record_limit)?)
                .min(65536),
        );
        let combined = (
            prior
                .0
                .checked_add(lease.0)
                .context("Task birth capture prior bytes overflow")?,
            prior
                .1
                .checked_add(lease.1)
                .context("Task birth capture prior nodes overflow")?,
        );
        let (selected, cost) =
            tasks.capture_query_with_prior(session.lease.selection(), limits, combined)?;
        let material = (
            cost.0
                .checked_add(lease.0)
                .context("Task birth selected bytes overflow")?,
            cost.1
                .checked_add(lease.1)
                .context("Task birth selected nodes overflow")?,
        );
        Ok((
            selected,
            TaskBirthCaptureMaterial {
                material,
                _lease: session.lease.0.clone(),
            },
        ))
    }
    pub(crate) fn task_birth_session_ids(
        &self,
        session: &TaskBirthSession,
    ) -> Result<(Vec<String>, TaskQueryOwnedRecords)> {
        let capture = self.capture_task_birth_session(session)?;
        let mut bytes = 128u64;
        let mut count = 0usize;
        for id in capture.born_task_ids_after(session.cursor)? {
            bytes = bytes
                .checked_add(serialized_bytes(&id)?)
                .and_then(|n| n.checked_add(128))
                .context("RunOnce Task ID bytes overflow")?;
            count = count
                .checked_add(1)
                .context("RunOnce Task ID count overflow")?;
        }
        let guard = self.task_birth_owned_budget(&capture.scope, bytes, count)?;
        let ids = capture
            .born_task_ids_after(session.cursor)?
            .map(str::to_owned)
            .collect();
        Ok((
            ids,
            TaskQueryOwnedRecords {
                _capture: capture,
                _owned: guard,
            },
        ))
    }
    pub(crate) fn task_birth_session_records(
        &self,
        session: &TaskBirthSession,
    ) -> Result<(Vec<crate::types::TaskRecord>, TaskQueryOwnedRecords)> {
        let capture = self.capture_task_birth_session(session)?;
        let mut bytes = 128u64;
        let mut count = 0usize;
        for id in capture.born_task_ids_after(session.cursor)? {
            let record = super::task_reads::task_record(&capture, id)?
                .context("RunOnce accepted Task body is missing")?;
            bytes = bytes
                .checked_add(serialized_bytes(record)?)
                .and_then(|n| n.checked_add(std::mem::size_of::<crate::types::TaskRecord>() as u64))
                .context("RunOnce Task record bytes overflow")?;
            count = count
                .checked_add(1)
                .context("RunOnce Task record count overflow")?;
        }
        let guard = self.task_birth_owned_budget(&capture.scope, bytes, count)?;
        let records = capture
            .born_task_ids_after(session.cursor)?
            .map(|id| {
                super::task_reads::task_record(&capture, id).and_then(|record| {
                    record
                        .cloned()
                        .context("RunOnce accepted Task body is missing")
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((
            records,
            TaskQueryOwnedRecords {
                _capture: capture,
                _owned: guard,
            },
        ))
    }
    pub(crate) fn task_session_id_budget<'a>(
        &self,
        session: &TaskBirthSession,
        ids: impl Iterator<Item = &'a str>,
    ) -> Result<Arc<BudgetViewGuard>> {
        let mut bytes = 128u64;
        let mut count = 0usize;
        for id in ids {
            bytes = bytes
                .checked_add(serialized_bytes(&id)?)
                .and_then(|n| n.checked_add(128))
                .context("RunOnce extra Task ID bytes overflow")?;
            count = count
                .checked_add(1)
                .context("RunOnce extra Task ID count overflow")?;
        }
        self.task_birth_owned_budget(&session.lease.0.scope, bytes, count)
    }
    pub(crate) fn task_session_wire_budget<T: serde::Serialize>(
        &self,
        session: &TaskBirthSession,
        value: &T,
    ) -> Result<Arc<BudgetViewGuard>> {
        self.task_birth_owned_budget(&session.lease.0.scope, serialized_bytes(value)?, 1)
    }
    fn task_birth_owned_budget(
        &self,
        scope: &InputEvidenceScope,
        bytes: u64,
        count: usize,
    ) -> Result<Arc<BudgetViewGuard>> {
        let bytes = bytes
            .checked_mul(4)
            .context("RunOnce Task copy budget overflow")?;
        let records = count
            .checked_mul(4)
            .context("RunOnce Task copy count overflow")?;
        let limits = self.inner.input_evidence_budget.limits();
        anyhow::ensure!(
            bytes
                <= limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("RunOnce Task limits overflow")?
                && u64::try_from(records)? <= limits.record_limit,
            TaskReadError::ResourceExhausted("RunOnce owned Task projection")
        );
        Ok(self.inner.input_evidence_budget.register_owned_bytes(
            scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?)
    }
}

#[cfg(test)]
mod birth_session_tests {
    use super::*;
    use crate::{runtime::tests::support::LifecycleHarness, types::*};
    fn task(id: &str) -> TaskRecord {
        TaskRecord {
            id: id.into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Queued,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: None,
            recovery: None,
        }
    }
    #[tokio::test]
    async fn run_once_birth_sessions_and_status_are_pure_after_task_identity_state_tables_are_forbidden(
    ) -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let (_, first) = runtime.begin_task_birth_session().await?;
        assert!(runtime.task_birth_session_ids(&first)?.0.is_empty());
        runtime
            .persist_task_transition(&task("first-session-task"), "task_queued")
            .await?;
        let (_, later) = runtime.begin_task_birth_session().await?;
        runtime
            .persist_task_transition(&task("later-session-task"), "task_queued")
            .await?;
        let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        runtime.runtime_db().transaction(|tx| {
            tx.execute_batch(
                "ALTER TABLE tasks RENAME TO tasks_run_once_forbidden;
                ALTER TABLE task_result_settlements RENAME TO results_run_once_forbidden;
                ALTER TABLE agent_states RENAME TO states_run_once_forbidden;
                ALTER TABLE agent_identities RENAME TO identities_run_once_forbidden;",
            )?;
            Ok(())
        })?;
        let (first_ids, first_hold) = runtime.task_birth_session_ids(&first)?;
        let (later_ids, later_hold) = runtime.task_birth_session_ids(&later)?;
        assert_eq!(first_ids, vec!["first-session-task", "later-session-task"]);
        assert_eq!(later_ids, vec!["later-session-task"]);
        assert_eq!(
            runtime.current_task_output_status("later-session-task")?,
            TaskStatus::Queued
        );
        let (_, empty) = runtime.begin_task_birth_session().await?;
        assert!(runtime.task_birth_session_ids(&empty)?.0.is_empty());
        let (records, records_hold) = runtime.task_birth_session_records(&first)?;
        assert_eq!(records.len(), 2);
        assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
        drop((
            records,
            records_hold,
            first_ids,
            first_hold,
            later_ids,
            later_hold,
            empty,
        ));
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        Ok(())
    }
    #[tokio::test]
    async fn held_report_task_registration_uses_exact_root_and_capture_is_pure() -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let reservation = runtime.inner.mutation_owner.reserve().await?;
        let root = reservation
            .capture_ready_snapshot()?
            .context("Root missing")?;
        let first = runtime.register_task_birth_session_from_snapshot(&reservation, &root)?;
        let same = runtime.register_task_birth_session_from_snapshot(&reservation, &root)?;
        assert_eq!(first.cursor, same.cursor);
        let other = LifecycleHarness::new();
        let other_reservation = other.runtime().inner.mutation_owner.reserve().await?;
        let other_root = other_reservation
            .capture_ready_snapshot()?
            .context("Other root missing")?;
        assert!(runtime
            .register_task_birth_session_from_snapshot(&other_reservation, &other_root)
            .is_err());
        assert!(runtime
            .capture_task_birth_session_from_snapshot(&first, &other_root)
            .is_err());
        drop(other_reservation);
        assert!(runtime
            .capture_task_birth_session_from_snapshot(&first, &root)?
            .born_task_ids_after(first.cursor)?
            .next()
            .is_none());
        drop(reservation);
        runtime
            .persist_task_transition(&task("held-report-new-task"), "task_queued")
            .await?;
        let reservation = runtime.inner.mutation_owner.reserve().await?;
        let current = reservation
            .capture_ready_snapshot()?
            .context("Current root missing")?;
        let error = runtime
            .register_task_birth_session_from_snapshot(&reservation, &root)
            .err()
            .context("stale baseline must reject")?;
        assert!(error.to_string().contains("captured publication differs"));
        runtime.runtime_db().connection()?.execute_batch(
            "ALTER TABLE tasks RENAME TO held_report_tasks_forbidden;
             ALTER TABLE task_result_settlements RENAME TO held_report_results_forbidden;
             ALTER TABLE agent_states RENAME TO held_report_states_forbidden;
             ALTER TABLE agent_identities RENAME TO held_report_identities_forbidden;",
        )?;
        let selected = runtime.capture_task_birth_session_from_snapshot(&first, &current)?;
        assert_eq!(
            selected
                .born_task_ids_after(first.cursor)?
                .collect::<Vec<_>>(),
            vec!["held-report-new-task"]
        );
        assert!(Arc::ptr_eq(
            &current,
            &reservation.capture_ready_snapshot()?.unwrap()
        ));
        drop((selected, same, first, reservation));
        Ok(())
    }
    #[tokio::test]
    async fn birth_session_cannot_capture_a_different_runtime_with_the_same_scoped_identity(
    ) -> Result<()> {
        let first = LifecycleHarness::new();
        let other = LifecycleHarness::new();
        let (_, session) = first.runtime().begin_task_birth_session().await?;
        let error = other
            .runtime()
            .capture_task_birth_session(&session)
            .unwrap_err();
        assert!(error.to_string().contains("another runtime scope or owner"));
        Ok(())
    }
    #[tokio::test]
    async fn output_readiness_uses_received_status_metadata_without_message_body_sql() -> Result<()>
    {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let task = task("received-readiness-task");
        let mut source = MessageEnvelope::new(
            "default",
            MessageKind::TaskStatus,
            MessageOrigin::Task {
                task_id: task.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "large output is not a status read".repeat(4096),
            },
        )
        .with_admission(
            MessageDeliverySurface::TaskRejoin,
            AdmissionContext::RuntimeOwned,
        );
        source.task_id = Some(task.id.clone());
        source.metadata = Some(
            serde_json::json!({"task_id":task.id,"task_kind":"command_task","task_status":"completed"}),
        );
        runtime
            .persist_task_transition_with_message(&task, "task_queued", &source)
            .await?;
        runtime.runtime_db().transaction(|tx| {tx.execute_batch("ALTER TABLE messages RENAME TO messages_status_body_forbidden;ALTER TABLE tasks RENAME TO tasks_status_body_forbidden;")?;Ok(())})?;
        assert_eq!(
            runtime.current_task_output_status(&task.id)?,
            TaskStatus::Completed
        );
        Ok(())
    }
    #[tokio::test]
    async fn completed_reply_owner_uses_complete_relation_original_order_and_one_pure_root(
    ) -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let first = runtime
            .create_work_item("first reply owner".into(), None, None, vec![])
            .await?;
        let second = runtime
            .create_work_item("second reply owner".into(), None, None, vec![])
            .await?;
        let role = runtime.register_task_query(TaskQuerySpec {
            reply_message_ids: &["reply-evidence", "empty-reply"],
            ..Default::default()
        })?;
        let base: chrono::DateTime<chrono::Utc> = "2026-10-01T00:00:00.123Z".parse()?;
        for index in 0..132 {
            let mut row = task(&format!("z-reply-{index:03}"));
            row.kind = TaskKind::AgentMessageWait;
            row.status = TaskStatus::Completed;
            row.created_at = base + chrono::Duration::nanoseconds(100);
            row.updated_at = row.created_at;
            row.work_item_id = Some(first.id.clone());
            row.detail = Some(serde_json::json!({"message_id":"reply-evidence"}));
            if index == 0 {
                row.id = "a-first".into();
                row.created_at = base + chrono::Duration::nanoseconds(900);
                row.work_item_id = Some(second.id.clone());
            } else if index == 1 {
                row.created_at = base - chrono::Duration::seconds(2);
                row.work_item_id = None;
                row.detail = Some(
                    serde_json::json!({"message_id":"reply-evidence","work_item_id":first.id}),
                );
            } else if index == 2 {
                row.created_at = base - chrono::Duration::seconds(1);
                row.work_item_id = Some("absent-open-owner".into());
            }
            runtime
                .persist_task_transition(&row, "task_completed")
                .await
                .with_context(|| format!("actual completed reply owner Task seed {index}"))?;
        }
        let expected = runtime
            .runtime_db()
            .tasks()
            .message_waits_for_reply("default", "reply-evidence")?
            .into_iter()
            .filter_map(|task| task.work_item_id)
            .find(|id| id == &first.id || id == &second.id);
        assert_eq!(expected.as_deref(), Some(second.id.as_str()));
        let mut owner = runtime.begin_mutation().await?;
        owner.prepare_owner_task_query(&role).await
            .context("actual completed reply owner Task named preparation")?;
        owner
            .prepare_owner_work_request(&super::super::work_reads::live_work_request())
            .await
            .context("actual completed reply owner Work named preparation")?;
        let (actual, _actual_budget) = owner.capture_reply_work_owner(&role, "reply-evidence")?;
        assert_eq!(actual, expected);
        assert!(owner
            .capture_reply_work_owner(&role, "empty-reply")?
            .0
            .is_none());
        assert!(owner
            .capture_reply_work_owner(&role, "unobserved-reply")
            .is_err());
        runtime.runtime_db().transaction(|tx| {
            tx.execute_batch(
                "ALTER TABLE tasks RENAME TO reply_tasks_forbidden;
                ALTER TABLE work_items RENAME TO reply_work_forbidden;
                ALTER TABLE agent_states RENAME TO reply_state_forbidden;
                ALTER TABLE agent_identities RENAME TO reply_identity_forbidden;",
            )?;
            Ok(())
        })?;
        assert_eq!(
            owner.capture_reply_work_owner(&role, "reply-evidence")?.0,
            expected
        );
        Ok(())
    }
}

#[path = "task_query_recovery.rs"]
mod recovery;

#[cfg(test)]
#[path = "task_query_recovery_tests.rs"]
mod recovery_tests;

#[cfg(test)]
mod birth_capture_joint_tests {
    use super::*;
    use crate::runtime::tests::support::LifecycleHarness;
    use crate::types::{TaskKind, TaskRecord, TaskStatus};
    fn task(id: &str, detail: Option<serde_json::Value>) -> TaskRecord {
        let at = chrono::Utc::now();
        TaskRecord {
            id: id.into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Queued,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail,
            recovery: None,
        }
    }
    #[tokio::test]
    async fn birth_task_capture_admits_held_lease_and_actual_caller_before_any_selected_clone(
    ) -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        runtime
            .persist_task_transition(
                &task(
                    "before-task-birth",
                    Some(serde_json::json!({"large":"x".repeat(256 * 1024)})),
                ),
                "task_queued",
            )
            .await?;
        let (_, session) = runtime.begin_task_birth_session().await?;
        runtime
            .persist_task_transition(&task("after-task-birth", None), "task_queued")
            .await?;
        // Hold the refresh lock only after foreground writes have finished.
        let _pause = runtime.pause_resource_observer_for_test().await;
        let root = runtime
            .inner
            .mutation_owner
            .capture()?
            .context("published root missing")?;
        let ledger = &runtime.inner.input_evidence_budget;
        let (selected, hold) = runtime.capture_task_birth_session_from_snapshot_with_prior(
            &session,
            &root,
            (64 * 1024 * 1024, 65536),
            (0, 0),
        )?;
        assert_eq!(selected.records.len(), 1);
        assert_eq!(selected.records[0].locator.key.id, "after-task-birth");
        let actual = hold.material_estimate()?;
        let original_arc = selected.records[0].clone();
        assert!(
            actual.0 < 256 * 1024,
            "unselected pre-birth body leaked into this phase"
        );
        drop((selected, hold));
        let prior = (256 * 1024u64, 32usize);
        let guard = ledger.register_owned_bytes(
            &session.lease.0.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: prior.0,
                metadata_estimate_bytes: 0,
            },
        )?;
        let held = vec![7u8; prior.0 as usize];
        let baseline = ledger.report()?.observed_bytes;
        let total = (usize::try_from(actual.0 + prior.0)?, actual.1 + prior.1);
        runtime.runtime_db().connection()?.execute_batch(
            "ALTER TABLE tasks RENAME TO birth_joint_forbidden_tasks;
             ALTER TABLE task_result_settlements RENAME TO birth_joint_forbidden_results;
             ALTER TABLE agent_states RENAME TO birth_joint_forbidden_states;
             ALTER TABLE agent_identities RENAME TO birth_joint_forbidden_identities;",
        )?;
        for limits in [(total.0 - 1, total.1), (total.0, total.1 - 1)] {
            let error = runtime
                .capture_task_birth_session_from_snapshot_with_prior(&session, &root, limits, prior)
                .err()
                .context("joint Task capture admitted too much material")?;
            assert!(
                matches!(
                    error.downcast_ref::<TaskReadError>(),
                    Some(TaskReadError::ResourceExhausted(_))
                ),
                "{error:#}"
            );
            assert_eq!(ledger.report()?.observed_bytes, baseline);
        }
        let (selected, material) = runtime
            .capture_task_birth_session_from_snapshot_with_prior(&session, &root, total, prior)?;
        assert_eq!(
            material.material_estimate()?,
            actual,
            "caller prior must not enter returned own material"
        );
        assert!(Arc::ptr_eq(&selected.records[0], &original_arc));
        let weak = Arc::downgrade(&session.lease.0);
        drop(session);
        assert!(
            weak.upgrade().is_some(),
            "capture material dropped the actual lease before wire"
        );
        drop((selected, material));
        assert!(weak.upgrade().is_none());
        drop((guard, held));
        Ok(())
    }
    #[tokio::test]
    async fn birth_task_capture_rejects_foreign_session_and_ledger_without_a_new_guard(
    ) -> Result<()> {
        let first = LifecycleHarness::new();
        let other = LifecycleHarness::new();
        let _pause_first = first.runtime().pause_resource_observer_for_test().await;
        let _pause_other = other.runtime().pause_resource_observer_for_test().await;
        let (_, session) = first.runtime().begin_task_birth_session().await?;
        let root = first.runtime().inner.mutation_owner.capture()?.unwrap();
        let foreign_root = other.runtime().inner.mutation_owner.capture()?.unwrap();
        let baseline = first
            .runtime()
            .inner
            .input_evidence_budget
            .report()?
            .observed_bytes;
        assert!(first
            .runtime()
            .capture_task_birth_session_from_snapshot_with_prior(
                &session,
                &foreign_root,
                (64 * 1024 * 1024, 65536),
                (0, 0)
            )
            .is_err());
        let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
        let forged = TaskBirthSession {
            cursor: session.cursor,
            lease: first.runtime().inner.task_queries.register(
                &session.lease.0.scope,
                root.token.owner_epoch,
                TaskQuerySpec {
                    birth_after: Some(session.cursor),
                    ..Default::default()
                },
                &foreign,
                65536,
                64 * 1024 * 1024,
            )?,
        };
        assert!(first
            .runtime()
            .capture_task_birth_session_from_snapshot_with_prior(
                &forged,
                &root,
                (64 * 1024 * 1024, 65536),
                (0, 0)
            )
            .is_err());
        assert_eq!(
            first
                .runtime()
                .inner
                .input_evidence_budget
                .report()?
                .observed_bytes,
            baseline
        );
        drop(forged);
        assert_eq!(foreign.report()?.observed_bytes, 0);
        Ok(())
    }
}
