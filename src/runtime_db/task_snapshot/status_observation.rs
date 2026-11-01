//! Selected TaskStatus metadata uses accepted headers, never canonical tails.
use super::*;
use crate::runtime::input_evidence_budget::serialized_bytes;
use crate::types::{TaskKind, TaskRecord, TaskStatus};
use serde_json::Value;

pub(super) fn child_id(task: &TaskRecord) -> Option<&str> {
    let detail = task.detail.as_ref()?;
    let id = detail.get("child_agent_id")?.as_str()?;
    (task.kind.is_child_agent()
        || (task.kind == TaskKind::ActorInvocation
            && detail.get("created_new_subagent").and_then(Value::as_bool) == Some(true)))
    .then_some(id)
}

pub(in crate::runtime_db) fn observe_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    selected: &[Arc<QualifiedTaskFact>],
    mut charge: impl FnMut(usize, usize) -> Result<()>,
) -> Result<TaskStatusProjectionCoverage> {
    let (control_complete, delegation_complete): (bool, bool) = tx.query_row(
        "SELECT control_observed,delegation_observed FROM task_status_source_inventory WHERE singleton=1 AND sealed=1 AND length(metadata_digest)=64", [], |row| Ok((row.get(0)?,row.get(1)?)),
    )?;
    let mut coverage = TaskStatusProjectionCoverage::default();
    for record in selected {
        let CanonicalTaskFact::Task(task) = &record.fact else {
            continue;
        };
        ensure!(
            record.locator.scope == *scope && task.agent_id == scope.agent_id,
            "TaskStatus observation crossed its selected Task scope"
        );
        charge(
            usize::try_from(serialized_bytes(&record.locator)?)?
                .checked_add(task.id.len() + 256)
                .context("TaskStatus head bytes overflow")?,
            20,
        )?;
        let worktree = observe_worktree(tx, scope, task, control_complete, &mut charge)?;
        let delegation = observe_delegation(tx, task, delegation_complete, &mut charge)?;
        ensure!(
            coverage
                .heads
                .insert(
                    task.id.clone(),
                    TaskStatusProjectionObservation {
                        task: record.locator.clone(),
                        worktree,
                        delegation,
                    }
                )
                .is_none(),
            "duplicate selected TaskStatus observation"
        );
    }
    Ok(coverage)
}

fn observe_worktree(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    task: &TaskRecord,
    complete: bool,
    charge: &mut impl FnMut(usize, usize) -> Result<()>,
) -> Result<TaskWorktreeObservation> {
    let artifact = task.detail.as_ref().and_then(|v| v.get("worktree"));
    if !matches!(
        task.status,
        TaskStatus::Completed
            | TaskStatus::Failed
            | TaskStatus::Cancelled
            | TaskStatus::Interrupted
    ) || !artifact.is_some_and(Value::is_object)
    {
        return Ok(TaskWorktreeObservation::Observed {
            fields: BTreeMap::new(),
        });
    }
    if !complete {
        return Ok(TaskWorktreeObservation::Unavailable);
    }
    let (count,bytes):(usize,usize)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(target_generation_json AS BLOB))+length(CAST(fields_json AS BLOB))+256),0) FROM task_status_control_sources WHERE agent_id=?1 AND identity_incarnation=?2 AND task_id=?3",params![scope.agent_id,scope.identity_incarnation,task.id],|row|Ok((row.get(0)?,row.get(1)?)))?;
    // The loader's charge_ids owns four copies. Two units cover raw JSON,
    // decoded generation/fields and the merged projection simultaneously.
    charge(
        bytes
            .checked_mul(2)
            .context("TaskStatus cleanup bytes overflow")?,
        count
            .checked_mul(2)
            .context("TaskStatus cleanup count overflow")?,
    )?;
    let nodes:usize=tx.query_row("SELECT COALESCE(SUM((SELECT COUNT(*) FROM json_tree(s.target_generation_json))+(SELECT COUNT(*) FROM json_tree(s.fields_json))),0) FROM task_status_control_sources s WHERE agent_id=?1 AND identity_incarnation=?2 AND task_id=?3",params![scope.agent_id,scope.identity_incarnation,task.id],|row|row.get(0))?;
    charge(
        0,
        nodes
            .checked_mul(2)
            .context("TaskStatus cleanup node count overflow")?,
    )?;
    charge(
        usize::try_from(serialized_bytes(&artifact)?)?
            .checked_add(256)
            .context("TaskStatus original artifact bytes overflow")?,
        1,
    )?;
    let created_at = serde_json::to_value(task.created_at)?;
    let mut query=tx.prepare("SELECT target_generation_json,fields_json FROM task_status_control_sources WHERE agent_id=?1 AND identity_incarnation=?2 AND task_id=?3 ORDER BY updated_at ASC,command_id ASC")?;
    let mut rows = query.query(params![scope.agent_id, scope.identity_incarnation, task.id])?;
    let mut fields = BTreeMap::new();
    let mut read = 0usize;
    while let Some(row) = rows.next()? {
        let generation: Value = serde_json::from_str(&row.get::<_, String>(0)?)?;
        let saved: BTreeMap<String, Value> = serde_json::from_str(&row.get::<_, String>(1)?)?;
        ensure!(
            saved
                .keys()
                .all(|name| TASK_WORKTREE_CLEANUP_FIELDS.contains(&name.as_str())),
            "TaskStatus source contains non-cleanup metadata"
        );
        read = read
            .checked_add(1)
            .context("TaskStatus source row count overflow")?;
        if generation.get("created_at") == Some(&created_at)
            && rejoin_matches(&generation, task)
            && generation.get("artifact") == artifact
        {
            fields.extend(saved);
        }
    }
    ensure!(
        read == count,
        "TaskStatus cleanup source set changed within one view"
    );
    Ok(TaskWorktreeObservation::Observed { fields })
}

/// New targets bind the planned terminal shape; old exact receipts stay immutable.
fn rejoin_matches(generation: &Value, task: &TaskRecord) -> bool {
    crate::runtime::task_generation::control_terminal_rejoin_matches(task, generation)
}

fn observe_delegation(
    tx: &Transaction<'_>,
    task: &TaskRecord,
    complete: bool,
    charge: &mut impl FnMut(usize, usize) -> Result<()>,
) -> Result<TaskDelegationObservation> {
    let Some(child) = child_id(task) else {
        return Ok(TaskDelegationObservation::Observed { latest: None });
    };
    if !complete {
        return Ok(TaskDelegationObservation::Unavailable);
    }
    let size:Option<usize>=tx.query_row("SELECT length(CAST(delegation_id AS BLOB))+length(CAST(child_agent_id AS BLOB))+length(CAST(parent_work_item_id AS BLOB))+length(CAST(child_work_item_id AS BLOB))+256 FROM task_status_delegation_sources WHERE child_agent_id=?1 ORDER BY updated_at DESC,created_at DESC,delegation_id ASC LIMIT 1",[child],|row|row.get(0)).optional()?;
    let Some(size) = size else {
        return Ok(TaskDelegationObservation::Observed { latest: None });
    };
    charge(size, 7)?;
    let latest:TaskDelegationMetadata=tx.query_row("SELECT child_agent_id,parent_work_item_id,child_work_item_id,delegation_id FROM task_status_delegation_sources WHERE child_agent_id=?1 ORDER BY updated_at DESC,created_at DESC,delegation_id ASC LIMIT 1",[child],|row|Ok(TaskDelegationMetadata {child_agent_id:row.get(0)?,parent_work_item_id:row.get(1)?,child_work_item_id:row.get(2)?,delegation_id:row.get(3)?}))?;
    ensure!(
        latest.child_agent_id == child,
        "TaskStatus delegation source crossed original child"
    );
    Ok(TaskDelegationObservation::Observed {
        latest: Some(latest),
    })
}

/// Discovery only. Exact aliases and the current owner still authorize actors.
pub(in crate::runtime_db) fn sync_child_observers_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    selected: &[Arc<QualifiedTaskFact>],
) -> Result<()> {
    tx.execute(
        "DELETE FROM task_status_child_observers WHERE agent_id=?1 AND identity_incarnation=?2",
        params![scope.agent_id, scope.identity_incarnation],
    )?;
    for record in selected {
        let CanonicalTaskFact::Task(task) = &record.fact else {
            continue;
        };
        let Some(child) = child_id(task) else {
            continue;
        };
        ensure!(
            record.locator.scope == *scope && task.agent_id == scope.agent_id,
            "TaskStatus observer index crossed scope"
        );
        let changed=tx.execute("INSERT INTO task_status_child_observers(agent_id,identity_incarnation,task_id,child_agent_id,task_revision,content_digest,projection_digest) SELECT ?1,?2,?3,?4,?5,?6,?7 WHERE EXISTS(SELECT 1 FROM task_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family='task' AND record_id=?3 AND availability='present' AND revision=?5 AND content_digest=?6 AND projection_digest=?7)",params![scope.agent_id,scope.identity_incarnation,task.id,child,record.locator.revision,record.locator.content_digest,record.locator.projection_digest])?;
        ensure!(
            changed == 1,
            "TaskStatus child observer lacks its original qualified Task alias"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;
    use crate::types::{WorkItemDelegationRecord, WorkItemDelegationState};

    fn selected(tx: &Transaction<'_>) -> Result<Arc<QualifiedTaskFact>> {
        let scope = super::super::tests::scope();
        let mut task = super::super::tests::task("selected-status", TaskStatus::Cancelled);
        task.kind = TaskKind::WorktreeSubagentTask;
        task.detail = Some(
            serde_json::json!({"child_agent_id":"shared-child","worktree":{"path":"/original","branch":"original"}}),
        );
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &task.id)]);
        let proof = prepare_task_write_tx(tx, &scope, &keys)?;
        repositories::upsert_task_tx(tx, &task)?;
        let facts = register_task_writes_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new(&scope.agent_id),
            &super::super::tests::request(),
        )?;
        Ok(facts
            .delta
            .records
            .into_iter()
            .find(|r| r.locator.key.id == task.id)
            .unwrap())
    }

    #[test]
    fn selected_status_merges_original_cleanup_and_global_delegation_without_canonical_queries(
    ) -> Result<()> {
        let (_home, db) = super::super::tests::fresh()?;
        let scope = super::super::tests::scope();
        let task = db.transaction(selected)?;
        let CanonicalTaskFact::Task(body) = &task.fact else {
            unreachable!()
        };
        let generation = serde_json::json!({"created_at":body.created_at,"rejoin":body.rejoin_fence().ok(),"artifact":body.detail.as_ref().unwrap()["worktree"]});
        db.transaction(|tx| {
            for (command,fields) in [("first",serde_json::json!({"cleanup_status":"retained","changed_files":["original.rs"]})),("later",serde_json::json!({"cleanup_reason":"original reason"}))] {
                tx.execute("INSERT INTO task_status_control_sources VALUES(?1,?2,?3,'worktree',?4,?5,?6,?7)",params![scope.agent_id,scope.identity_incarnation,command,body.id,generation.to_string(),fields.to_string(),repositories::timestamp(body.updated_at)])?;
            }
            Ok(())
        })?;
        for id in ["z-latest", "a-latest"] {
            let record = WorkItemDelegationRecord {
                delegation_id: id.into(),
                parent_agent_id: "another-parent".into(),
                parent_work_item_id: id.into(),
                child_agent_id: "shared-child".into(),
                child_work_item_id: "child-work".into(),
                state: WorkItemDelegationState::Open,
                result_summary: None,
                created_at: body.created_at,
                updated_at: body.updated_at,
            };
            db.work_item_delegations().upsert(&record)?;
        }
        db.transaction(|tx| {
            tx.execute_batch("ALTER TABLE control_cancellation_obligations RENAME TO status_control_body_forbidden;ALTER TABLE work_item_delegations RENAME TO status_delegation_body_forbidden;")?;
            Ok(())
        })?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let request = super::super::tests::request();
        let (coverage, guards) = db.transaction(|tx| {
            let mut material =
                read_preparation::TaskReadMaterial::new(&scope, &request, Some(&ledger));
            let coverage = observe_tx(tx, &scope, std::slice::from_ref(&task), |bytes, count| {
                material.charge_ids(bytes, count)
            })?;
            sync_child_observers_tx(tx, &scope, std::slice::from_ref(&task))?;
            Ok((coverage, material.into_guards()))
        })?;
        let head = &coverage.heads[&body.id];
        let observed = head.overlay(&task)?;
        assert_eq!(
            observed.detail.as_ref().unwrap()["worktree"],
            serde_json::json!({"path":"/original","branch":"original","cleanup_status":"retained","changed_files":["original.rs"],"cleanup_reason":"original reason"})
        );
        assert_eq!(
            head.child_supervision(&task)?
                .unwrap()
                .parent_work_item_id
                .as_deref(),
            Some("a-latest")
        );
        assert!(ledger.report()?.observed_bytes > 0);
        drop((coverage, guards));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn nested_status_metadata_is_admitted_before_rust_decode() -> Result<()> {
        let (_home, db) = super::super::tests::fresh()?;
        let scope = super::super::tests::scope();
        let record = db.transaction(selected)?;
        let CanonicalTaskFact::Task(task) = &record.fact else {
            unreachable!()
        };
        let generation = serde_json::json!({"created_at":task.created_at,"rejoin":task.rejoin_fence().ok(),"artifact":task.detail.as_ref().unwrap()["worktree"]});
        db.transaction(|tx| {
            tx.execute("INSERT INTO task_status_control_sources VALUES(?1,?2,'nested','worktree',?3,?4,?5,?6)",params![scope.agent_id,scope.identity_incarnation,task.id,generation.to_string(),serde_json::json!({"changed_files":vec!["file";256]}).to_string(),repositories::timestamp(task.updated_at)])?;
            Ok(())
        })?;
        let error = db
            .transaction(|tx| {
                observe_tx(tx, &scope, std::slice::from_ref(&record), |_, count| {
                    ensure!(count < 256, "native nested node admission before decode");
                    Ok(())
                })
            })
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("native nested node admission before decode"));
        Ok(())
    }
    #[test]
    fn borrowed_rejoin_comparison_preserves_null_exact_fields_and_long_parent_contract() {
        let mut task = super::super::tests::task("borrowed-fence", TaskStatus::Cancelled);
        assert!(rejoin_matches(&serde_json::json!({"rejoin":null}), &task));
        assert!(!rejoin_matches(&serde_json::json!({}), &task));
        task.detail = Some(
            serde_json::json!({"rejoin_obligation_id":task.id,"rejoin_generation":7,"parent_turn_id":"parent".repeat(16_384)}),
        );
        let mut generation = serde_json::json!({"rejoin":task.rejoin_fence().ok()});
        assert!(rejoin_matches(&generation, &task));
        generation["rejoin"]["unrelated"] = serde_json::json!(true);
        assert!(!rejoin_matches(&generation, &task));
        generation["rejoin"]
            .as_object_mut()
            .unwrap()
            .remove("unrelated");
        generation["rejoin"]["generation"] = serde_json::json!(8);
        assert!(!rejoin_matches(&generation, &task));
        task.detail.as_mut().unwrap()["rejoin_generation"] = serde_json::json!(0);
        assert!(
            !rejoin_matches(&serde_json::json!({"rejoin":null}), &task),
            "an invalid marker is not a legacy unbound metadata authority"
        );
    }
}
