//! An original metadata actor changes only already observed exact Task heads.
use super::*;
use crate::runtime::input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger};

/// The original Source actor owns scope/admission and whole receipt ordering.
/// It calls this only for Current, prepared heads selected under held owners.
/// Source-only / NoState / retired actors must retain their unavailable ACK.
pub(in crate::runtime_db) fn refresh_task_status_metadata_tx<'a>(
    tx: &Transaction<'_>,
    scope: &'a InputEvidenceScope,
    expected_task_revision: u64,
    selected: &[Arc<QualifiedTaskFact>],
    request: &'a TaskDomainLoadRequest,
    ledger: &'a InputEvidenceBudgetLedger,
    memory: Option<&'a mut super::super::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<Option<(CommittedTaskDelta, Vec<Arc<BudgetViewGuard>>)>> {
    let mut material = read_preparation::TaskReadMaterial::new(scope, request, Some(ledger))
        .with_memory_budget(memory);
    let delta = refresh_task_status_metadata_using_material_tx(
        tx,
        scope,
        expected_task_revision,
        selected,
        scope,
        request,
        ledger,
        &mut material,
    )?;
    Ok(delta.map(|delta| (delta, material.into_guards())))
}

/// One original multi-owner phase lends the same remaining allowance to every
/// affected scope. This does not prepare a missing domain or authorize a head.
pub(in crate::runtime_db) fn refresh_task_status_metadata_using_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    expected_task_revision: u64,
    selected: &[Arc<QualifiedTaskFact>],
    original_scope: &InputEvidenceScope,
    original_request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    material: &mut read_preparation::TaskReadMaterial<'_>,
) -> Result<Option<CommittedTaskDelta>> {
    material.validate_original_allowance(original_scope, original_request, ledger)?;
    let Some(mut coverage) = material.read_current_metadata_for_scope(tx, scope)? else {
        return Ok(None);
    };
    ensure!(
        coverage.revision == expected_task_revision,
        TaskSnapshotError::RevisionMismatch {
            expected: expected_task_revision,
            actual: Some(coverage.revision)
        }
    );
    material.charge_ids(
        selected
            .len()
            .checked_mul(std::mem::size_of::<&str>())
            .context("TaskStatus selected slots overflow")?,
        selected.len(),
    )?;
    let mut seen = BTreeSet::new();
    for record in selected {
        ensure!(
            record.locator.scope == *scope && record.locator.key.family == TaskFamily::Task,
            "TaskStatus metadata actor selected a foreign scope or family"
        );
        material.charge_record(&record.locator.key, record.resident_bytes)?;
        record.validate(scope)?;
        let head = coverage
            .status
            .heads
            .get(&record.locator.key.id)
            .ok_or(TaskReadError::IncompleteStatusProjection)?;
        ensure!(
            head.task == record.locator
                && coverage.observations.get(&record.locator.key)
                    == Some(&TaskAvailability::Present),
            "TaskStatus metadata actor selected another original Task head"
        );
        ensure!(
            seen.insert(record.locator.key.id.as_str()),
            "TaskStatus metadata actor selected a duplicate Task"
        );
        let exact:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM task_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family='task' AND record_id=?3 AND availability='present' AND revision=?4 AND content_digest=?5 AND projection_digest=?6)",params![scope.agent_id,scope.identity_incarnation,record.locator.key.id,record.locator.revision,record.locator.content_digest,record.locator.projection_digest],|row|row.get(0))?;
        ensure!(
            exact,
            "TaskStatus metadata actor original Task alias changed"
        );
    }
    let observed = status_observation::observe_tx(tx, scope, selected, |bytes, count| {
        material.charge_ids(bytes, count)
    })?;
    if observed
        .heads
        .iter()
        .all(|(id, head)| coverage.status.heads.get(id) == Some(head))
    {
        return Ok(None);
    }
    for (id, head) in observed.heads {
        coverage.status.heads.insert(id, head);
    }
    let base_revision = coverage.revision;
    coverage.revision = base_revision
        .checked_add(1)
        .context("TaskStatus metadata revision overflow")?;
    material.charge_final_metadata(
        read_preparation::encoded_metadata_bytes(&coverage)?,
        coverage.owned_metadata_record_count()?,
    )?;
    loader::persist_coverage(tx, scope, &coverage)?;
    Ok(Some(CommittedTaskDelta {
        scope: scope.clone(),
        base_revision,
        revision: coverage.revision,
        coverage,
        records: Vec::new(),
        stop_records: Vec::new(),
        removals: BTreeMap::new(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TaskKind, TaskStatus, WorkItemDelegationRecord, WorkItemDelegationState};

    fn fixture(
        tx: &Transaction<'_>,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(Arc<QualifiedTaskFact>, TaskCoverage)> {
        let scope = super::super::tests::scope();
        let request = super::super::tests::request();
        let mut task = super::super::tests::task("metadata-selected", TaskStatus::Running);
        task.kind = TaskKind::ChildAgentTask;
        task.detail = Some(serde_json::json!({"child_agent_id":"metadata-child"}));
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &task.id)]);
        let proof = prepare_task_write_tx(tx, &scope, &keys)?;
        repositories::upsert_task_tx(tx, &task)?;
        let mut facts = register_task_writes_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new(&scope.agent_id),
            &request,
        )?;
        let record = facts
            .delta
            .records
            .iter()
            .find(|r| r.locator.key.id == task.id)
            .unwrap()
            .clone();
        let mut material = read_preparation::TaskReadMaterial::new(&scope, &request, Some(ledger));
        facts.delta.coverage.status = status_observation::observe_tx(
            tx,
            &scope,
            std::slice::from_ref(&record),
            |bytes, count| material.charge_ids(bytes, count),
        )?;
        loader::persist_coverage(tx, &scope, &facts.delta.coverage)?;
        Ok((record, facts.delta.coverage))
    }

    #[test]
    fn metadata_finalizer_preserves_task_stop_query_output_and_advances_only_once() -> Result<()> {
        let (_home, db) = super::super::tests::fresh()?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let scope = super::super::tests::scope();
        let request = super::super::tests::request();
        let (record, before) = db.transaction(|tx| fixture(tx, &ledger))?;
        let actual_before = db.tasks().latest(&record.locator.key.id)?.unwrap();
        let delegation = WorkItemDelegationRecord {
            delegation_id: "metadata-delegation".into(),
            parent_agent_id: "global-parent".into(),
            parent_work_item_id: "parent-work".into(),
            child_agent_id: "metadata-child".into(),
            child_work_item_id: "child-work".into(),
            state: WorkItemDelegationState::Open,
            result_summary: None,
            created_at: super::super::tests::at(),
            updated_at: super::super::tests::at(),
        };
        db.work_item_delegations().upsert(&delegation)?;
        let (delta, guards) = db
            .transaction(|tx| {
                refresh_task_status_metadata_tx(
                    tx,
                    &scope,
                    before.revision,
                    std::slice::from_ref(&record),
                    &request,
                    &ledger,
                    None,
                )
            })?
            .unwrap();
        assert_eq!(delta.revision, before.revision + 1);
        assert!(
            delta.records.is_empty() && delta.stop_records.is_empty() && delta.removals.is_empty()
        );
        let mut expected = before.clone();
        expected.revision = delta.revision;
        expected.status = delta.coverage.status.clone();
        assert_eq!(delta.coverage, expected);
        assert_eq!(
            db.tasks().latest(&record.locator.key.id)?.unwrap(),
            actual_before
        );
        assert_eq!(
            delta.coverage.status.heads[&record.locator.key.id]
                .child_supervision(&record)?
                .unwrap()
                .delegation_id
                .as_deref(),
            Some("metadata-delegation")
        );
        assert!(ledger.report()?.observed_bytes > 0);
        let repeat = db.transaction(|tx| {
            refresh_task_status_metadata_tx(
                tx,
                &scope,
                delta.revision,
                std::slice::from_ref(&record),
                &request,
                &ledger,
                None,
            )
        })?;
        assert!(repeat.is_none());
        drop((delta, guards));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn metadata_finalizer_rejects_old_revision_and_never_bootstraps_task_none() -> Result<()> {
        let (_home, db) = super::super::tests::fresh()?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let scope = super::super::tests::scope();
        let request = super::super::tests::request();
        let (record, before) = db.transaction(|tx| fixture(tx, &ledger))?;
        let error = db
            .transaction(|tx| {
                refresh_task_status_metadata_tx(
                    tx,
                    &scope,
                    before.revision + 1,
                    std::slice::from_ref(&record),
                    &request,
                    &ledger,
                    None,
                )
            })
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::RevisionMismatch { .. })
        ));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        db.transaction(|tx| {
            tx.execute("DELETE FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation])?;
            assert!(refresh_task_status_metadata_tx(tx,&scope,before.revision,std::slice::from_ref(&record),&request,&ledger,None)?.is_none());
            assert!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2)",params![scope.agent_id,scope.identity_incarnation],|row|row.get::<_,bool>(0))?);
            Ok(())
        })?;
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
}

#[cfg(test)]
mod shared_material_tests {
    use super::*;
    use crate::types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
        TaskKind, TaskStatus, WorkItemDelegationRecord, WorkItemDelegationState,
    };
    fn selected(
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        id: &str,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(Arc<QualifiedTaskFact>, TaskCoverage)> {
        let request = super::super::tests::request();
        let mut task = super::super::tests::task(id, TaskStatus::Running);
        task.agent_id = scope.agent_id.clone();
        task.kind = TaskKind::ChildAgentTask;
        task.summary = Some("same-phase owned Task source".repeat(128));
        task.detail = Some(serde_json::json!({"child_agent_id":"shared-status-child"}));
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, id)]);
        let proof = prepare_task_write_tx(tx, scope, &keys)?;
        repositories::upsert_task_tx(tx, &task)?;
        let mut facts = register_task_writes_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new(&scope.agent_id),
            &request,
        )?;
        let record = facts
            .delta
            .records
            .iter()
            .find(|r| r.locator.key.id == id)
            .unwrap()
            .clone();
        let mut material = read_preparation::TaskReadMaterial::new(scope, &request, Some(ledger));
        facts.delta.coverage.status = status_observation::observe_tx(
            tx,
            scope,
            std::slice::from_ref(&record),
            |bytes, count| material.charge_ids(bytes, count),
        )?;
        loader::persist_coverage(tx, scope, &facts.delta.coverage)?;
        status_observation::sync_child_observers_tx(tx, scope, std::slice::from_ref(&record))?;
        Ok((record, facts.delta.coverage))
    }
    fn delegation() -> WorkItemDelegationRecord {
        WorkItemDelegationRecord {
            delegation_id: "shared-status-source".into(),
            parent_agent_id: "global-original-parent".into(),
            parent_work_item_id: "parent-work".into(),
            child_agent_id: "shared-status-child".into(),
            child_work_item_id: "child-work".into(),
            state: WorkItemDelegationState::Completed,
            result_summary: Some("actual original completion".into()),
            created_at: super::super::tests::at(),
            updated_at: super::super::tests::at(),
        }
    }
    #[test]
    fn two_observer_scopes_share_one_predecode_allowance_and_rollback_all_metadata_and_source(
    ) -> Result<()> {
        let (_home, db) = super::super::tests::fresh()?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let original = super::super::tests::scope();
        let other = InputEvidenceScope {
            agent_id: "second-status-owner".into(),
            identity_incarnation: 1,
        };
        db.agent_identities().upsert(&AgentIdentityRecord::new(
            &other.agent_id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))?;
        db.agent_states()
            .upsert_for_fixture(&AgentState::new(&other.agent_id))?;
        let (first, first_before) =
            db.transaction(|tx| selected(tx, &original, "shared-status-first", &ledger))?;
        let (second, second_before) =
            db.transaction(|tx| selected(tx, &other, "shared-status-second", &ledger))?;
        let request = super::super::tests::request();
        let estimates = db.transaction(|tx| {
            tx.execute_batch("SAVEPOINT measure_original_status")?;
            repositories::upsert_work_item_delegation_tx(tx, &delegation())?;
            let mut estimates = Vec::new();
            for (scope, record, revision) in [
                (&original, &first, first_before.revision),
                (&other, &second, second_before.revision),
            ] {
                let mut material =
                    read_preparation::TaskReadMaterial::new(&original, &request, Some(&ledger));
                assert!(refresh_task_status_metadata_using_material_tx(
                    tx,
                    scope,
                    revision,
                    std::slice::from_ref(record),
                    &original,
                    &request,
                    &ledger,
                    &mut material
                )?
                .is_some());
                estimates.push(material.material_estimate());
            }
            tx.execute_batch(
                "ROLLBACK TO measure_original_status; RELEASE measure_original_status",
            )?;
            Ok(estimates)
        })?;
        let mut limited = request.clone();
        limited.byte_limit = estimates.iter().map(|e| e.bytes).max().unwrap() + 128;
        limited.record_limit = 65536;
        assert!(estimates.iter().all(|e| e.bytes <= limited.byte_limit));
        assert!(estimates.iter().map(|e| e.bytes).sum::<usize>() > limited.byte_limit);
        let before = ledger.report()?.observed_bytes;
        let error = db
            .transaction(|tx| {
                repositories::upsert_work_item_delegation_tx(tx, &delegation())?;
                let mut material =
                    read_preparation::TaskReadMaterial::new(&original, &limited, Some(&ledger));
                for (scope, record, revision) in [
                    (&original, &first, first_before.revision),
                    (&other, &second, second_before.revision),
                ] {
                    refresh_task_status_metadata_using_material_tx(
                        tx,
                        scope,
                        revision,
                        std::slice::from_ref(record),
                        &original,
                        &limited,
                        &ledger,
                        &mut material,
                    )?;
                }
                Ok(())
            })
            .unwrap_err();
        assert!(error
            .downcast_ref::<TaskSnapshotError>()
            .is_some_and(|e| matches!(e, TaskSnapshotError::ResourceExhausted(_))));
        assert_eq!(ledger.report()?.observed_bytes, before);
        assert_eq!(
            db.transaction(|tx| loader::current_coverage(tx, &original))?,
            Some(first_before.clone())
        );
        assert_eq!(
            db.transaction(|tx| loader::current_coverage(tx, &other))?,
            Some(second_before.clone())
        );
        assert!(db
            .work_item_delegations()
            .latest_for_child("shared-status-child")?
            .is_none());
        let header_count:usize=db.connection()?.query_row("SELECT COUNT(*) FROM task_status_delegation_sources WHERE child_agent_id='shared-status-child'",[],|r|r.get(0))?;
        assert_eq!(header_count, 0);
        let (deltas, guards) = db.transaction(|tx| {
            repositories::upsert_work_item_delegation_tx(tx, &delegation())?;
            let mut material =
                read_preparation::TaskReadMaterial::new(&original, &request, Some(&ledger));
            let mut deltas = Vec::new();
            for (scope, record, revision) in [
                (&original, &first, first_before.revision),
                (&other, &second, second_before.revision),
            ] {
                deltas.push(
                    refresh_task_status_metadata_using_material_tx(
                        tx,
                        scope,
                        revision,
                        std::slice::from_ref(record),
                        &original,
                        &request,
                        &ledger,
                        &mut material,
                    )?
                    .unwrap(),
                );
            }
            Ok((deltas, material.into_guards()))
        })?;
        assert_eq!(deltas[0].revision, first_before.revision + 1);
        assert_eq!(deltas[1].revision, second_before.revision + 1);
        assert!(ledger.report()?.observed_bytes > before);
        drop((deltas, guards));
        assert_eq!(ledger.report()?.observed_bytes, before);
        Ok(())
    }
}
